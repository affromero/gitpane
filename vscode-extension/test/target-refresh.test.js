'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const { createHash } = require('node:crypto');

async function fixture(t) {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-target-refresh-'));
  t.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  const parent = path.join(workspace, 'parent');
  const child = path.join(workspace, 'child');
  const other = path.join(workspace, 'other');
  for (const root of [parent, child, other]) fs.mkdirSync(root);
  for (const root of [parent, other]) fs.mkdirSync(path.join(root, '.git'));
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const statusCalls = [], pages = [];
  const repos = [parent, child, other].map(root => ({ rootUri: uri(root),
    kind: root === child ? 'worktree' : 'repository',
    state: { onDidChange: event, HEAD: { name: 'main' }, indexChanges: [], workingTreeChanges: [], untrackedChanges: [] },
    async status() { statusCalls.push(root); } }));
  let childBranch = 'topic', onStatus, onPage;
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async status(root, includeWorktrees) {
      if (onStatus) await onStatus(root, includeWorktrees);
      return { common_dir: path.join(root === child ? parent : root, '.git'), branch: root === child ? childBranch : 'main',
        is_dirty: false, ahead: 0, behind: 0, stashes: [],
        worktrees: root === parent && includeWorktrees ? [{ path: child, branch: childBranch, is_dirty: false, file_count: 0 }] : [],
        has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [],
        file_count_lower_bound: 0, files_truncated: false };
    },
    async beginChanges(root) {
      pages.push(root);
      if (onPage) await onPage(root);
      return { snapshot: 'snapshot', revision: createHash('sha256').update(root).digest('hex'), cursor: 0,
        previous_cursor: null, next_cursor: null, changes: [] };
    },
    async releaseChanges() {} };
  const git = { repositories: repos, onDidOpenRepository: event, onDidCloseRepository: event };
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event } };
  const load = Module._load;
  let RepositoryTree, GitpaneViewProvider;
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree, GitpaneViewProvider } = require('../src/extension'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(git, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  t.after(() => { provider.dispose(); tree.dispose(); });
  await tree.refresh();
  const snapshot = provider.snapshot();
  const parentRow = snapshot.repositories.find(row => row.name === 'parent');
  const otherRow = snapshot.repositories.find(row => row.name === 'other');
  assert.ok(parentRow.worktrees[0]);
  statusCalls.length = 0;
  return { tree, provider, git, repos, parent, child, other, parentRow, otherRow, statusCalls, pages,
    setBranch(value) { childBranch = value; }, setOnStatus(value) { onStatus = value; },
    setOnPage(value) { onPage = value; } };
}

test('refreshing a worktree updates its anchor and open pages without scanning unrelated repositories', async t => {
  const f = await fixture(t);
  const worktree = f.parentRow.worktrees[0];
  await f.provider.loadChanges(f.parentRow.id);
  await f.provider.loadChanges(worktree.id);
  await f.provider.loadChanges(f.otherRow.id);
  f.pages.length = 0;
  f.setBranch('new-topic');
  await f.provider.refreshTarget(worktree.id, 'worktree');
  assert.deepEqual(new Set(f.statusCalls), new Set([f.parent, f.child]));
  assert.deepEqual(new Set(f.pages), new Set([f.parent, f.child]));
  assert.equal(f.provider.snapshot().repositories.find(row => row.name === 'parent').worktrees[0].branch, 'new-topic');
});

test('refresh waits for an in-flight page then requests a new snapshot', async t => {
  const f = await fixture(t);
  let release, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnPage(async () => {
    if (!release) { started(); await new Promise(resolve => { release = resolve; }); }
  });
  const old = f.provider.loadChanges(f.parentRow.id);
  await began;
  const refresh = f.provider.refreshTarget(f.parentRow.id, 'repo');
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(f.pages, [f.parent]);
  release();
  await Promise.all([old, refresh]);
  assert.deepEqual(f.pages, [f.parent, f.parent]);
});

test('refresh retries a failed backend and restores repository status', async t => {
  const f = await fixture(t);
  f.tree.backend.state = { status: 'error', error: new Error('backend stopped') };
  f.tree.backendChanged(f.tree.backend.state);
  let retried = false;
  f.tree.backend.retry = async () => {
    retried = true;
    f.tree.backendChanged({ status: 'initializing' });
    f.tree.backend.state = { status: 'ready' };
    f.tree.backendChanged(f.tree.backend.state);
  };
  await f.provider.refreshTarget(f.parentRow.id, 'repo');
  assert.equal(retried, true);
  assert.equal(f.tree.backendStatuses.get(f.parent)?.branch, 'main');
  assert.equal(f.tree.backendError, undefined);
});

test('a stale queued refresh does not block a later current repository', async t => {
  const f = await fixture(t);
  let release, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnStatus(async root => {
    if (root === f.other && !release) { started(); await new Promise(resolve => { release = resolve; }); }
  });
  const scan = f.tree.refresh();
  await began;
  const selected = f.provider.refreshTarget(f.parentRow.id, 'repo');
  const later = f.provider.refreshTarget(f.otherRow.id, 'repo');
  f.tree.unobserve(f.repos[0]);
  const replacement = { ...f.repos[0], state: { ...f.repos[0].state } };
  f.git.repositories[0] = replacement;
  f.tree.observe(replacement);
  f.statusCalls.length = 0;
  release();
  let timeout;
  try {
    await Promise.race([Promise.all([scan, selected, later]), new Promise((_, reject) => {
      timeout = setTimeout(() => reject(new Error('A current refresh remained queued behind a stale target.')), 5000);
    })]);
  } finally { clearTimeout(timeout); }
  assert.deepEqual(f.statusCalls, [f.other]);
});
