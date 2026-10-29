'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { WorkspaceFetch } = require('../src/workspace-fetch');

function fixture() {
  const root = path.resolve('fetch-workspace');
  const main = path.join(root, 'main');
  const linked = path.join(root, 'linked');
  const other = path.join(root, 'other');
  const common = path.join(root, 'common');
  let targets = [
    { path: main, commonDir: common, commonIdentity: 'same', repoIdentity: 'a'.repeat(64), guard: 'main:1' },
    { path: linked, commonDir: common, commonIdentity: 'same', repoIdentity: 'a'.repeat(64), guard: 'linked:1' },
    { path: other, commonDir: path.join(root, 'other-common'), commonIdentity: 'other', repoIdentity: 'a'.repeat(64), guard: 'other:1' }
  ];
  let preview = async repo => ({ repo_identity: 'a'.repeat(64), fetch_token: repo === linked ? 'b'.repeat(64) : 'c'.repeat(64) });
  let execute = async () => {};
  const calls = [], notices = [], logs = [], progress = [];
  let refreshes = 0;
  const tree = { fetchTargets: () => targets.map(target => ({ ...target })) };
  const backend = { generation: 1, previewFetch: repo => preview(repo) };
  const vscode = { ProgressLocation: { Notification: 1 }, window: {
    async withProgress(options, operation) { assert.equal(options.cancellable, false); return operation({ report: item => progress.push(item) }); },
    async showInformationMessage(message) { notices.push(message); },
    async showWarningMessage(message) { notices.push(message); }
  } };
  const controller = new WorkspaceFetch({ tree, vscode, output: { appendLine: line => logs.push(line), show() {} },
    backend,
    mutations: { execute: (...args) => { calls.push(args); return execute(...args); } },
    refresh: async () => { refreshes++; } });
  return { controller, root, main, linked, other, common, calls, notices, logs, progress,
    get refreshes() { return refreshes; }, setTargets(value) { targets = value; },
    setPreview(value) { preview = value; }, setExecute(value) { execute = value; },
    changeWorkspace() { backend.generation++; } };
}

test('explicit fetch runs every worktree with its own preview and serializes shared Git directories', async () => {
  const f = fixture();
  assert.deepEqual(f.calls, [], 'creating the controller never starts a fetch');
  const active = new Set();
  let maxParallel = 0;
  f.setExecute(async (_method, _params, options) => {
    assert.ok(!active.has(options.lockKey), 'worktrees sharing a Git directory must not overlap');
    assert.equal(options.waitForClose, true);
    assert.equal(options.validateTarget(), true);
    active.add(options.lockKey);
    maxParallel = Math.max(maxParallel, active.size);
    await new Promise(resolve => setTimeout(resolve, 10));
    active.delete(options.lockKey);
  });
  await f.controller.run();
  assert.equal(f.calls.length, 3);
  assert.ok(maxParallel > 1 && maxParallel <= 3);
  const linked = f.calls.find(([, params]) => params.repo === f.linked);
  assert.equal(linked[1].expected_fetch_token, 'b'.repeat(64));
  assert.ok(f.calls.every(([method]) => method === 'repo_fetch'));
  assert.equal(f.refreshes, 1);
  assert.equal(f.progress.length, 3);
  assert.match(f.notices.at(-1), /fetched 3 repositories/i);
});

test('workspace changes during preview stop the affected fetch and preserve partial results', async () => {
  const f = fixture();
  f.setPreview(async repo => {
    if (repo === f.main) f.setTargets([{ path: f.other, commonDir: path.join(f.root, 'other-common'), commonIdentity: 'other', repoIdentity: 'a'.repeat(64), guard: 'other:1' }]);
    return { repo_identity: 'a'.repeat(64), fetch_token: 'b'.repeat(64) };
  });
  await f.controller.run();
  assert.ok(!f.calls.some(([, params]) => params.repo === f.main || params.repo === f.linked));
  assert.equal(f.refreshes, 1);
  assert.match(f.notices.at(-1), /failed/i);
  assert.ok(f.logs.some(line => line.includes(f.main) && /changed/i.test(line)));
  assert.ok(f.logs.some(line => line.includes(f.linked) && /changed/i.test(line)));
});

test('fetch failures and unknown outcomes are reported per repository without stopping other groups', async () => {
  const f = fixture();
  f.setExecute(async (_method, params) => {
    if (params.repo === f.main) throw Object.assign(new Error('network interrupted'), { code: 'outcome_unknown' });
    if (params.repo === f.other) throw new Error('remote unavailable');
  });
  await f.controller.run();
  assert.equal(f.calls.length, 3);
  assert.ok(f.logs.some(line => line.includes(f.main) && /outcome is unknown/i.test(line)));
  assert.ok(f.logs.some(line => line.includes(f.other) && /remote unavailable/i.test(line)));
  assert.match(f.notices.at(-1), /fetched 1 of 3/i);
  assert.equal(f.refreshes, 1);
});

test('missing or malformed backend confirmation never dispatches fetch', async () => {
  const f = fixture();
  f.setPreview(async () => ({ repo_identity: 'a'.repeat(64), fetch_token: 'malformed' }));
  await f.controller.run();
  assert.deepEqual(f.calls, []);
  assert.equal(f.logs.length, 3);
  assert.match(f.notices.at(-1), /invalid fetch preview/i);
});

test('workspace reconfiguration during worker startup invalidates the original selection', async () => {
  const f = fixture();
  f.setExecute(async (_method, _params, options) => {
    f.changeWorkspace();
    assert.equal(options.validateTarget(), false);
    throw new Error('Workspace changed before fetch.');
  });
  await f.controller.run();
  assert.ok(f.logs.every(line => /changed/i.test(line)));
  assert.match(f.notices.at(-1), /failed/i);
});

test('a changed repository identity is rejected before any fetch can authorize it', async () => {
  const f = fixture();
  f.setPreview(async repo => ({ repo_identity: (repo === f.linked ? 'd' : 'a').repeat(64), fetch_token: 'b'.repeat(64) }));
  await f.controller.run();
  assert.ok(!f.calls.some(([, params]) => params.repo === f.linked));
  assert.ok(f.logs.some(line => line.includes(f.linked) && /changed/i.test(line)));
});

test('a worktree replaced while another fetch holds the shared lock keeps its original identity', async () => {
  const f = fixture();
  let release, entered;
  const firstStarted = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const previews = new Set();
  const executedGit = [];
  let linkedIdentity = 'a'.repeat(64);
  f.setPreview(async repo => {
    previews.add(repo);
    return { repo_identity: 'a'.repeat(64), fetch_token: 'b'.repeat(64) };
  });
  f.setExecute(async (_method, params) => {
    if (params.repo === f.main) { entered(); await held; }
    if (params.repo === f.linked && params.expected_repo_identity !== linkedIdentity) {
      throw Object.assign(new Error('Repository changed before fetch.'), { code: 'stale_target' });
    }
    executedGit.push(params.repo);
  });
  const run = f.controller.run();
  await firstStarted;
  assert.ok(previews.has(f.linked), 'all identities are captured before the first fetch starts');
  linkedIdentity = 'd'.repeat(64);
  release();
  await run;
  assert.ok(!executedGit.includes(f.linked), 'the replacement is not fetched');
  assert.ok(f.logs.some(line => line.includes(f.linked) && /changed/i.test(line)));
});
