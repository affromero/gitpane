'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');

function fixture(t, count = 2) {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-refresh-groups-'));
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const roots = Array.from({ length: count }, (_, index) => {
    const root = path.join(workspace, `repo-${String(index).padStart(2, '0')}`);
    fs.mkdirSync(path.join(root, '.git'), { recursive: true });
    return root;
  });
  const repos = roots.map(root => ({ rootUri: uri(root), kind: 'repository',
    state: { onDidChange: event, HEAD: { name: 'main' } }, async status() {} }));
  const common = new Map(roots.map(root => [root, path.join(root, '.git')]));
  const worktrees = new Map(roots.map(root => [root, []]));
  const calls = [], opens = [], output = [];
  let onStatus;
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async status(root, includeWorktrees, options) {
      calls.push({ root, includeWorktrees, watched: options.watchedPaths });
      if (onStatus) await onStatus(root, includeWorktrees);
      return { common_dir: common.get(root), branch: 'main', is_dirty: false, ahead: 0, behind: 0,
        stashes: [], worktrees: includeWorktrees ? worktrees.get(root) : [],
        has_dirty_submodules: false, has_unpushed_submodules: false,
        files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false };
    } };
  const api = { repositories: repos, onDidOpenRepository: event, onDidCloseRepository: event,
    async openRepository(value) { opens.push(value.fsPath); return undefined; } };
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri }, workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event } };
  const load = Module._load;
  let RepositoryTree;
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree } = require('../src/repository-tree'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine(message) { output.push(message); } }, backend);
  t.after(() => { tree.dispose(); fs.rmSync(workspace, { recursive: true, force: true }); });
  return { workspace, roots, repos, common, worktrees, calls, opens, output, tree,
    onStatus(callback) { onStatus = callback; } };
}

test('warmed independent repositories need one backend scan each', async t => {
  const f = fixture(t, 20);
  await f.tree.refresh();
  assert.equal(f.calls.length, 40, 'unknown repositories require a grouping scan and an anchor scan');
  f.calls.length = 0;
  await f.tree.refresh();
  assert.equal(f.calls.length, 20, 'each independent repository is scanned once after grouping');
  assert.ok(f.calls.every(call => call.includeWorktrees));
  assert.equal(f.tree.backendStatuses.size, 20);
});

test('worktree discovery waits for regrouping and follows a merged or split group', async t => {
  const f = fixture(t);
  await f.tree.refresh();
  const [first, second] = f.roots;
  const correct = path.join(f.workspace, 'correct-worktree');
  const obsolete = path.join(f.workspace, 'obsolete-worktree');
  fs.mkdirSync(correct);
  fs.mkdirSync(obsolete);
  f.common.set(second, f.common.get(first));
  f.worktrees.set(first, [{ path: correct }]);
  f.worktrees.set(second, [{ path: obsolete }]);
  let release, started;
  const began = new Promise(resolve => { started = resolve; });
  f.onStatus(async root => {
    if (root === first && !release) { started(); await new Promise(resolve => { release = resolve; }); }
  });
  const refresh = f.tree.refresh();
  await began;
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(f.opens, [], 'a speculative anchor cannot open worktrees while a sibling is pending');
  release();
  await refresh;
  assert.deepEqual(f.opens, [correct]);
  assert.deepEqual(f.tree.backendStatuses.get(first).worktrees, [{ path: correct }]);
  f.onStatus(undefined);
  f.opens.length = 0;
  f.calls.length = 0;
  f.common.set(second, path.join(second, '.git'));
  await f.tree.refresh();
  assert.deepEqual(f.opens.sort(), [correct, obsolete].sort());
  assert.deepEqual(f.calls.filter(call => call.root === second).map(call => call.includeWorktrees), [false, true],
    'a newly independent repository obtains its own worktree list');
});

test('a replaced common directory is grouped again before worktree discovery', async t => {
  const f = fixture(t, 1);
  await f.tree.refresh();
  const gitDir = f.common.get(f.roots[0]);
  fs.renameSync(gitDir, `${gitDir}-old`);
  fs.mkdirSync(gitDir);
  f.calls.length = 0;
  await f.tree.refresh();
  assert.deepEqual(f.calls.map(call => call.includeWorktrees), [false, true]);
});

test('a changed watched path or token forces a fresh detailed status', async t => {
  const f = fixture(t, 1);
  let watched = ['old'], token = 1;
  f.tree.watchedPathsFor = () => watched;
  f.tree.watchTokenFor = () => token;
  await f.tree.refresh();
  for (const next of [{ paths: ['new'], token: 2 }, { paths: [], token: 3 }, { paths: ['new'], token: 4 },
    { paths: ['new'], token: 5 }]) {
    let release, started;
    const began = new Promise(resolve => { started = resolve; });
    f.onStatus(async (_root, include) => {
      if (include && !release) { started(); await new Promise(resolve => { release = resolve; }); }
    });
    f.calls.length = 0;
    const refresh = f.tree.refresh();
    await began;
    watched = next.paths;
    token = next.token;
    const unblock = release;
    f.onStatus(undefined);
    unblock();
    await refresh;
    assert.deepEqual(f.calls.map(call => call.includeWorktrees), [true, true]);
    assert.deepEqual(f.tree.backendStatuses.get(f.roots[0]).watchedPaths, next.paths);
  }
});

test('a failed detailed scan is reported without restoring cached status', async t => {
  const f = fixture(t, 1);
  await f.tree.refresh();
  f.onStatus(async (_root, include) => { if (include) throw new Error('worktree status failed'); });
  f.calls.length = 0;
  await f.tree.refresh();
  assert.deepEqual(f.calls.map(call => call.includeWorktrees), [true]);
  assert.equal(f.tree.backendStatuses.has(f.roots[0]), false);
  assert.match(f.tree.backendErrors.get(f.roots[0]), /worktree status failed/);
  assert.ok(f.output.some(message => message.includes('worktree status failed')));
});

test('a replaced repository cannot reuse a held status or its group', async t => {
  const f = fixture(t, 1);
  await f.tree.refresh();
  let release, started;
  const began = new Promise(resolve => { started = resolve; });
  f.onStatus(async () => { if (!release) { started(); await new Promise(resolve => { release = resolve; }); } });
  const refresh = f.tree.refresh();
  await began;
  f.tree.unobserve(f.repos[0]);
  const replacement = { ...f.repos[0], state: { ...f.repos[0].state } };
  f.repos[0] = replacement;
  f.tree.observe(replacement);
  release();
  await refresh;
  assert.equal(f.tree.backendStatuses.has(f.roots[0]), false);
  f.onStatus(undefined);
  f.calls.length = 0;
  await f.tree.refresh();
  assert.deepEqual(f.calls.map(call => call.includeWorktrees), [false, true]);
});
