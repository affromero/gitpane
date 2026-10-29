'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { MutationManager } = require('../src/mutation-manager');

// A real process boundary lets the tests control when requests are received,
// while leaving the manager's transport and lifecycle behavior intact.
const service = String.raw`
const fs = require('node:fs');
const readline = require('node:readline');
const dir = process.env.MUTATION_TEST_DIR;
const mode = process.env.MUTATION_TEST_MODE;
const file = name => require('node:path').join(dir, name);
function release(name, fn) {
  const timer = setInterval(() => {
    if (fs.existsSync(file(name))) { clearInterval(timer); fn(); }
  }, 10);
}
function reply(value) { process.stdout.write(JSON.stringify(value) + '\n', () => {}); }
readline.createInterface({ input: process.stdin }).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    fs.writeFileSync(file('initialized'), '');
    const answer = () => reply({id: request.id, result: {version: mode === 'wrong-version' ? 2 : 1}});
    if (mode === 'hold-init') release('release-init', answer); else answer();
    return;
  }
  fs.appendFileSync(file('actions'), JSON.stringify(request) + '\n');
  if (mode === 'exit-after-action') { process.exit(7); return; }
  const answer = () => {
    if (mode === 'git-error') reply({id: request.id, error: {code: 'git_error', message: 'git failed'}});
    else reply({id: request.id, result: {}});
  };
  if (mode === 'hold-action') release('release-action', answer); else answer();
});
`;

async function waitFor(predicate, message) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error(message);
}

async function exists(file) {
  try { await fs.access(file); return true; } catch { return false; }
}

async function fixture(t, mode = 'success') {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'gitpane-mutation-test-'));
  const workspace = path.join(dir, 'workspace');
  const repo = path.join(workspace, 'repo');
  const linked = path.join(workspace, 'linked');
  const common = path.join(workspace, 'common');
  const outside = path.join(dir, 'outside');
  await Promise.all([repo, linked, common, outside].map(p => fs.mkdir(p, { recursive: true })));
  let generation = 1;
  const children = [];
  const manager = new MutationManager({
    getSnapshot: () => ({ executable: path.join(dir, 'backend'), roots: [workspace], generation }),
    isCurrent: snapshot => snapshot.generation === generation,
    spawn: (_executable, _args, options) => {
      const child = spawn(process.execPath, ['-e', service], {
        ...options, env: { ...process.env, MUTATION_TEST_DIR: dir, MUTATION_TEST_MODE: mode }
      });
      children.push({ child, closed: new Promise(resolve => child.once('close', resolve)) });
      return child;
    }
  });
  t.after(async () => {
    manager.dispose();
    await Promise.all(['release-init', 'release-action'].map(name => fs.writeFile(path.join(dir, name), '')));
    await Promise.all(children.map(({ closed }) => Promise.race([
      closed, new Promise((_, reject) => {
        const timer = setTimeout(() => reject(new Error('Mutation service did not exit')), 5000);
        timer.unref();
      })
    ])));
    await fs.rm(dir, { recursive: true, force: true });
  });
  const opts = { lockKey: common, validateTarget: () => true };
  return {
    dir, repo, linked, common, outside, manager, opts,
    changeWorkspace: () => { generation++; },
    mark: name => fs.writeFile(path.join(dir, name), ''),
    wait: name => waitFor(() => exists(path.join(dir, name)), `Timed out waiting for ${name}`),
    actions: async () => (await fs.readFile(path.join(dir, 'actions'), 'utf8')).trim().split('\n').map(JSON.parse)
  };
}

function code(expected) { return error => error.code === expected; }

test('requires a valid shared Git directory and rejects repositories outside the workspace', async t => {
  const f = await fixture(t);
  const action = { repo: f.repo, path: 'file', operation: 'stage' };
  await assert.rejects(f.manager.execute('file_action', action, { validateTarget: () => true }), code('invalid_request'));
  await assert.rejects(f.manager.execute('file_action', action, { ...f.opts, lockKey: 'relative' }), code('invalid_request'));
  await assert.rejects(f.manager.execute('file_action', action, { ...f.opts, lockKey: path.join(f.dir, 'missing') }), code('invalid_request'));
  const file = path.join(f.dir, 'file');
  await fs.writeFile(file, '');
  await assert.rejects(f.manager.execute('file_action', action, { ...f.opts, lockKey: file }), code('invalid_request'));
  await assert.rejects(f.manager.execute('file_action', { ...action, repo: f.outside }, f.opts), code('outside_workspace'));
  assert.equal(await exists(path.join(f.dir, 'actions')), false);
});

test('sends a literal file action once and returns its result', async t => {
  const f = await fixture(t);
  const params = { repo: f.repo, path: ':(glob)*.txt', operation: 'stage' };
  assert.deepEqual(await f.manager.execute('file_action', params, f.opts), {});
  const actions = await f.actions();
  assert.equal(actions.length, 1);
  assert.equal(actions[0].method, 'file_action');
  assert.deepEqual(actions[0].params, { ...params, repo: await fs.realpath(f.repo) });
});

test('destructive file actions require the confirmation identity', async t => {
  const f = await fixture(t);
  const params = { repo: f.repo, path: 'file', operation: 'discard' };
  await assert.rejects(f.manager.execute('file_action', params, f.opts), code('invalid_request'));
  await assert.rejects(f.manager.execute('file_action', { ...params, expected_token: 'bad' }, f.opts), code('invalid_request'));
  assert.equal(await exists(path.join(f.dir, 'actions')), false);
  const token = 'a'.repeat(64);
  assert.deepEqual(await f.manager.execute('file_action', { ...params, expected_token: token }, f.opts), {});
  assert.equal((await f.actions())[0].params.expected_token, token);
});

test('sends a sync action with the selected branch and HEAD identity', async t => {
  const f = await fixture(t);
  const params = { repo: f.repo, operation: 'pull', expected_branch: 'main', expected_head: 'a'.repeat(40) };
  assert.deepEqual(await f.manager.execute('repo_sync', params, f.opts), {});
  const actions = await f.actions();
  assert.equal(actions.length, 1);
  assert.equal(actions[0].method, 'repo_sync');
  assert.deepEqual(actions[0].params, { ...params, repo: await fs.realpath(f.repo) });
});

test('submodule batches carry the selected repository HEAD identity', async t => {
  const f = await fixture(t);
  const params = { repo: f.repo, operation: 'update', expected_branch: 'main', expected_head: 'a'.repeat(40) };
  assert.deepEqual(await f.manager.execute('submodule_action', params, f.opts), {});
  const actions = await f.actions();
  assert.equal(actions.length, 1);
  assert.equal(actions[0].method, 'submodule_action');
  assert.deepEqual(actions[0].params, { ...params, repo: await fs.realpath(f.repo) });
});

test('sends a selected session as a literal one-shot request', async t => {
  const f = await fixture(t);
  const params = { repo: f.repo, backend: 'herdr', session: 'tab $(not-a-command)' };
  assert.deepEqual(await f.manager.execute('session_goto', params, f.opts), {});
  const actions = await f.actions();
  assert.equal(actions.length, 1);
  assert.equal(actions[0].method, 'session_goto');
  assert.deepEqual(actions[0].params, { ...params, repo: await fs.realpath(f.repo) });
});

test('workspace changes during initialization cancel the action before dispatch', async t => {
  const f = await fixture(t, 'hold-init');
  const result = f.manager.execute('file_action', { repo: f.repo, path: 'file', operation: 'stage' }, f.opts);
  await f.wait('initialized');
  f.changeWorkspace();
  await f.mark('release-init');
  await assert.rejects(result, code('workspace_changed'));
  assert.equal(await exists(path.join(f.dir, 'actions')), false);
});

test('a stale selected target cancels the action before dispatch', async t => {
  const f = await fixture(t);
  await assert.rejects(f.manager.execute('file_action', { repo: f.repo, path: 'file', operation: 'discard', expected_token: 'a'.repeat(64) }, {
    ...f.opts, validateTarget: () => false
  }), code('workspace_changed'));
  assert.equal(await exists(path.join(f.dir, 'actions')), false);
});

test('a dispatched action drains after workspace changes and blocks linked worktree actions', async t => {
  const f = await fixture(t, 'hold-action');
  const first = f.manager.execute('file_action', { repo: f.repo, path: 'file', operation: 'stage' }, f.opts);
  await f.wait('actions');
  await assert.rejects(f.manager.execute('file_action', { repo: f.linked, path: 'other', operation: 'stage' }, f.opts), code('busy'));
  f.changeWorkspace();
  f.manager.dispose();
  await f.mark('release-action');
  assert.deepEqual(await first, {});
  assert.equal((await f.actions()).length, 1);
});

test('transport loss after dispatch reports unknown outcome without replaying', async t => {
  const f = await fixture(t, 'exit-after-action');
  await assert.rejects(f.manager.execute('file_action', { repo: f.repo, path: 'file', operation: 'discard', expected_token: 'a'.repeat(64) }, f.opts), code('outcome_unknown'));
  assert.equal((await f.actions()).length, 1);
});

test('a reported Git failure remains explicit and is never replayed', async t => {
  const f = await fixture(t, 'git-error');
  await assert.rejects(f.manager.execute('worktree_remove', { repo: f.repo, path: f.linked }, f.opts), code('git_error'));
  assert.equal((await f.actions()).length, 1);
});

test('unsupported service versions fail before dispatch', async t => {
  const f = await fixture(t, 'wrong-version');
  await assert.rejects(f.manager.execute('file_action', { repo: f.repo, path: 'file', operation: 'stage' }, f.opts), code('unsupported_version'));
  assert.equal(await exists(path.join(f.dir, 'actions')), false);
});
