'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const { isolatedGitEnv } = require('./integration/git-env');
const { SidebarActions } = require('../src/sidebar-actions');

function change(group, file, kind) {
  return { group, path: file, kind, original_path: null, conflict_stages: null, is_submodule: false };
}

function changesPage(changes) {
  return { snapshot: 'fixture-snapshot', revision: createHash('sha256').update(JSON.stringify(changes)).digest('hex'), cursor: 0,
    previous_cursor: null, next_cursor: null, changes };
}

test('status refresh retains the watched paths sent with each request', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-watch-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, '.git'));
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const repo = { rootUri: uri(root), state: { onDidChange: event }, async status() {} };
  const api = { repositories: [repo], onDidOpenRepository: event, onDidCloseRepository: event };
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    workspace: { workspaceFolders: [{ uri: uri(root) }], onDidChangeWorkspaceFolders: event } };
  let release;
  const calls = [];
  let watchErrors = [];
  const backend = { state: { status: 'ready' }, onDidChange: event, async status(value, worktrees, options) {
    calls.push(options);
    await new Promise(resolve => { release = resolve; });
    return { common_dir: path.join(root, '.git'), branch: 'main', is_dirty: false, ahead: 0, behind: 0,
      stashes: [], worktrees: [], has_dirty_submodules: false, has_unpushed_submodules: false,
      files: [], watched_errors: watchErrors, file_count_lower_bound: 0, files_truncated: false };
  } };
  const load = Module._load;
  let RepositoryTree;
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree } = require('../src/repository-tree'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  t.after(() => tree.dispose());
  let watched = ['old'];
  tree.watchedPathsFor = () => watched;
  let token = 1;
  tree.watchTokenFor = () => token;
  const cycle = { generation: tree.backendGeneration, tokens: new Map([[root, tree.repositoryToken(repo)]]) };
  const pending = tree.refreshBackend(repo, cycle, false);
  watched = ['new'];
  token = 2;
  release();
  await pending;
  assert.deepEqual(calls[0], { summaryOnly: true, watchedPaths: ['old'] });
  assert.deepEqual(tree.backendStatuses.get(root).watchedPaths, ['old']);
  assert.equal(tree.backendStatuses.get(root).watchTokens.get('old'), 1);
  watchErrors = [{ path: 'new', code: 'outside_workspace', message: 'Path leaves the workspace' }];
  const errored = tree.refreshBackend(repo, cycle, false);
  release();
  await errored;
  assert.deepEqual(tree.backendStatuses.get(root).watched_errors, watchErrors);
  assert.equal(tree.backendErrors.size, 0);
  watchErrors = [{ path: 'unrequested', code: 'outside_workspace', message: 'Path leaves the workspace' }];
  const invalid = tree.refreshBackend(repo, cycle, false);
  release();
  await invalid;
  assert.equal(tree.backendStatuses.has(root), false);
  assert.match(tree.backendErrors.get(root), /invalid compact repository status/i);
});

test('workspace authorization follows the backend identity captured during initialization', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-shared-roots-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'workspace');
  const saved = path.join(root, 'original');
  fs.mkdirSync(source);
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    workspace: { workspaceFolders: [{ uri: uri(source) }], onDidChangeWorkspaceFolders: event } };
  const backend = { workspaceIdentities: new Map(), state: { status: 'initializing' }, onDidChange: event };
  const api = { repositories: [], onDidOpenRepository: event, onDidCloseRepository: event };
  const load = Module._load;
  let RepositoryTree;
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree } = require('../src/repository-tree'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  t.after(() => tree.dispose());
  assert.equal(backend.workspaceIdentities.has(source), false, 'the tree must not capture an independent identity');
  fs.renameSync(source, saved);
  fs.mkdirSync(source);
  const real = fs.realpathSync.native(source);
  const stat = fs.statSync(real, { bigint: true });
  backend.workspaceIdentities.set(source, { path: process.platform === 'win32' ? real.toLowerCase() : real,
    dev: stat.dev, ino: stat.ino });
  assert.equal(tree.isPathInWorkspace(source), true);
  assert.ok(!tree.getChildren().some(node => node.type === 'error'));
});

test('disposing settles refresh callers waiting for a poll or initial status', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-refresh-dispose-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, '.git'));
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    workspace: { workspaceFolders: [{ uri: uri(root) }], onDidChangeWorkspaceFolders: event } };
  const load = Module._load;
  let RepositoryTree;
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree } = require('../src/repository-tree'));
  } finally { Module._load = load; }
  for (const mode of ['poll', 'status']) {
    let enterStatus;
    const statusStarted = new Promise(resolve => { enterStatus = resolve; });
    const backend = { state: { status: 'ready' }, onDidChange: event, async status() {
      enterStatus();
      return new Promise(() => {});
    } };
    const repo = { rootUri: uri(root), state: { onDidChange: event }, async status() {} };
    const api = { repositories: [repo], onDidOpenRepository: event, onDidCloseRepository: event };
    const tree = new RepositoryTree(api, { appendLine() {} }, backend);
    if (mode === 'poll') tree.pollPromise = new Promise(() => {});
    const pending = tree.refresh();
    if (mode === 'status') await statusStarted;
    else await new Promise(resolve => setImmediate(resolve));
    tree.dispose();
    let timeout;
    try {
      await Promise.race([pending, new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`Disposed refresh waiting for ${mode} did not settle`)), 5000);
      })]);
    } finally { clearTimeout(timeout); }
  }
});

test('slow detailed refresh finishes one sweep before scheduling another', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-page-sweep-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const roots = ['a', 'b'].map(name => path.join(temporary, name));
  for (const root of roots) fs.mkdirSync(path.join(root, '.git'), { recursive: true });
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const repos = roots.map(root => ({ rootUri: uri(root), state: { HEAD: { name: 'main' }, onDidChange: event },
    async status() {} }));
  let hold = false;
  let release;
  const calls = [];
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async status(root) { return { common_dir: path.join(root, '.git'), branch: 'main', is_dirty: false,
      stashes: [], worktrees: [], ahead: 0, behind: 0, has_dirty_submodules: false, has_unpushed_submodules: false,
      files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false }; },
    async beginChanges(root) {
    calls.push(root);
    if (hold && root === roots[0]) await new Promise(resolve => { release = resolve; });
    return changesPage([]);
  }, async releaseChanges() {} };
  const git = { repositories: repos, onDidOpenRepository: event, onDidCloseRepository: event };
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') },
    workspace: { workspaceFolders: [{ uri: uri(temporary) }], onDidChangeWorkspaceFolders: event } };
  const load = Module._load;
  let GitpaneViewProvider, RepositoryTree;
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ GitpaneViewProvider, RepositoryTree } = require('../src/extension'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(git, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  let receive;
  const view = { visible: true, webview: { postMessage() {}, onDidReceiveMessage(handler) { receive = handler; return event(); } },
    onDidChangeVisibility: event, onDidDispose: event };
  try {
    Module._load = function(name, ...args) {
      if (name === './webview/html') return { getHtml: () => '' };
      return load.call(this, name, ...args);
    };
    provider.resolveWebviewView(view);
  } finally { Module._load = load; }
  try {
    await tree.refresh();
    await receive({ type: 'ready' });
    const owners = provider.snapshot().repositories.map(repo => repo.id);
    for (const owner of owners) await provider.loadChanges(owner);
    calls.length = 0;
    hold = true;
    t.mock.timers.tick(5000);
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(calls, [roots[0]]);
    t.mock.timers.tick(10000);
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(calls, [roots[0]]);
    hold = false;
    release();
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(calls, roots);
  } finally { provider.dispose(); tree.dispose(); }
});

test('reopening a repository invalidates pending status, selections, and menus', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-reopen-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const workspace = path.join(temporary, 'workspace');
  const root = path.join(workspace, 'repo');
  fs.mkdirSync(path.join(root, '.git'), { recursive: true });
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const makeRepo = () => ({ rootUri: uri(root), state: { onDidChange: event, HEAD: { name: 'main' },
    workingTreeChanges: [{ uri: uri(path.join(root, 'file.txt')), status: 5 }] }, async status() {} });
  let onOpen, onClose, releaseStatus, releaseMenu;
  let holdStatus = false;
  const status = branch => ({ common_dir: path.join(root, '.git'), branch, head_oid: 'a'.repeat(40),
    is_dirty: true, ahead: 0, behind: 0, stashes: [], worktrees: [],
    has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [], file_count_lower_bound: 1, files_truncated: true });
  const api = { repositories: [makeRepo()],
    onDidOpenRepository(handler) { onOpen = handler; return { dispose() {} }; },
    onDidCloseRepository(handler) { onClose = handler; return { dispose() {} }; } };
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} }, Uri: { file: uri },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event },
    window: { showQuickPick: choices => new Promise(resolve => { releaseMenu = () => resolve(choices.find(item => item.action === 'copyRepo')); }),
      showErrorMessage: async message => { throw new Error(message); } },
    env: { clipboard: { writeText: async value => copied.push(value) } } };
  const copied = [];
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async status() { if (holdStatus) return new Promise(resolve => { releaseStatus = () => resolve(status('old-response')); }); return status('main'); },
    async beginChanges() { return changesPage([change('working', 'file.txt', 'working_modified')]); },
    async releaseChanges() {} };
  const load = Module._load;
  let RepositoryTree, GitpaneViewProvider;
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    ({ RepositoryTree, GitpaneViewProvider } = require('../src/extension'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  const actions = new SidebarActions({ vscode, resolveRepository: id => provider.resolveRepository(id),
    resolveChange: id => provider.resolveChange(id), resolveWorktree: id => provider.resolveWorktree(id) });
  try {
    await tree.refresh();
    await provider.loadChanges(provider.snapshot().repositories[0].id);
    for (const replaceDirectory of [false, true]) {
      const before = provider.snapshot().repositories[0];
      const fileId = before.groups[0].files[0].id;
      const oldNode = provider.changes.get(fileId);
      const menu = actions.showMenu('repo', before.id);
      holdStatus = true;
      const pending = tree.refresh();
      while (!releaseStatus) await new Promise(resolve => setImmediate(resolve));
      const oldRepo = api.repositories[0];
      const revision = tree.repositoryRevision(oldRepo.rootUri);
      api.repositories = [];
      onClose(oldRepo);
      if (replaceDirectory) {
        fs.renameSync(root, path.join(workspace, 'old-repo'));
        fs.mkdirSync(path.join(root, '.git'), { recursive: true });
      }
      const reopened = makeRepo();
      api.repositories = [reopened];
      onOpen(reopened);
      assert.equal(tree.repositoryRevision(reopened.rootUri), revision + 2);
      releaseStatus();
      releaseStatus = undefined;
      holdStatus = false;
      releaseMenu();
      await Promise.all([pending, menu]);
      assert.equal(tree.backendStatuses.get(root), undefined);
      assert.equal(provider.currentChange(oldNode), undefined);
      assert.equal(provider.currentRepository(before.id), undefined);
      assert.deepEqual(copied, []);
      await tree.refresh();
      const refreshed = provider.snapshot().repositories[0];
      await provider.loadChanges(refreshed.id);
      const after = provider.snapshot().repositories[0];
      assert.equal(after.branch, 'main');
      assert.notEqual(after.id, before.id);
      assert.notEqual(after.groups[0].files[0].id, fileId);
    }
  } finally {
    provider.dispose();
    tree.dispose();
    delete require.cache[require.resolve('../src/extension')];
    delete require.cache[require.resolve('../src/repository-tree')];
  }
});

test('workspace repositories appear automatically and reconcile while visible', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-tree-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const workspace = path.join(temporary, 'workspace');
  const otherWorkspace = path.join(temporary, 'other-workspace');
  const root = path.join(workspace, 'repo');
  const addedRoot = path.join(workspace, 'nested', 'repo');
  const otherRoot = path.join(otherWorkspace, 'other');
  for (const directory of [root, addedRoot, otherRoot, path.join(otherWorkspace, 'external')]) {
    fs.mkdirSync(directory, { recursive: true });
  }
  for (const directory of [root, addedRoot, otherRoot]) fs.mkdirSync(path.join(directory, '.git'));
  let failed = false;
  let backendFailed = false;
  let addedFile = false;
  let sortFailure = false;
  let holdSort = false;
  let releaseSort;
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const repo = {
    rootUri: uri(root),
    state: {
      onDidChange: event, HEAD: { name: 'main' },
      indexChanges: [{ uri: uri(path.join(root, 'file')), status: 0 }],
      workingTreeChanges: [{ uri: uri(path.join(root, 'file')), status: 5 }]
    },
    async status() {
      if (failed) throw new Error('status failed');
      if (addedFile) this.state.workingTreeChanges.push({ uri: uri(path.join(root, 'new')), status: 7 });
    }
  };
  const added = {
    rootUri: uri(addedRoot), kind: 'worktree',
    state: { onDidChange: event, HEAD: { name: 'feature' } },
    async status() {}
  };
  const other = {
    rootUri: uri(otherRoot), state: { onDidChange: event, HEAD: { name: 'main' } },
    async status() {}
  };
  let onOpen;
  let onClose;
  let onWorkspaceChange;
  const api = {
    repositories: [repo, other],
    onDidOpenRepository(handler) { onOpen = handler; return { dispose() {} }; },
    onDidCloseRepository(handler) { onClose = handler; return { dispose() {} }; }
  };
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri },
    workspace: {
      workspaceFolders: [{ uri: uri(workspace) }],
      onDidChangeWorkspaceFolders(handler) { onWorkspaceChange = handler; return { dispose() {} }; }
    }
  };
  const backend = {
    state: { status: 'ready' },
    onDidChange: event,
    async beginChanges(repoPath) {
      if (repoPath !== root) return changesPage([]);
      const files = [change('index', 'file', 'index_modified'), change('working', 'file', 'working_modified')];
      if (addedFile) files.push(change('untracked', 'new', 'untracked'));
      return changesPage(files);
    },
    async releaseChanges() {},
    async status(repoPath, includeWorktrees = true) {
      if (backendFailed) throw new Error('backend status failed');
      return {
        common_dir: path.join(repoPath, '.git'),
        branch: repoPath === root ? 'main' : 'feature', is_dirty: repoPath === root,
        ahead: 2, behind: 1, stashes: [{ index: 0 }],
        worktrees: includeWorktrees ? [{ path: addedRoot }, { path: path.join(otherWorkspace, 'external') }] : [],
        has_dirty_submodules: true, has_unpushed_submodules: true,
        files: [], watched_errors: [], file_count_lower_bound: repoPath === root ? 1 : 0, files_truncated: repoPath === root
      };
    },
    async sortRepositories(order, repositories) {
      if (sortFailure) throw new Error('sort service failed');
      assert.deepEqual(new Set(repositories.map(item => item.path)), new Set([root, addedRoot]));
      if (holdSort && order === 'alphabetical') await new Promise(resolve => { releaseSort = resolve; });
      const first = repositories.findIndex(item => item.path === (order === 'alphabetical' ? addedRoot : root));
      return { indices: [first, 1 - first] };
    }
  };
  const load = Module._load;
  let RepositoryTree, GitpaneViewProvider;
  try {
    Module._load = function (name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ RepositoryTree, GitpaneViewProvider } = require('../src/extension'));
  } finally {
    Module._load = load;
  }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  const countFiles = repo => new Set(repo.groups.flatMap(group => group.files.map(file => file.path))).size;
  const settle = () => new Promise(resolve => setImmediate(resolve));
  try {
    await tree.refresh();
    assert.deepEqual(tree.getChildren().filter(node => node.type === 'repo').map(node => node.repo.rootUri.fsPath), [root]);
    await provider.loadChanges(provider.snapshot().repositories[0].id);
    assert.equal(countFiles(provider.snapshot().repositories[0]), 1);
    const initial = provider.snapshot().repositories[0];
    assert.equal(initial.ahead, 2);
    assert.equal(initial.behind, 1);
    assert.equal(initial.stashCount, 1);
    assert.equal(initial.worktreeCount, 1);
    assert.equal(initial.hasDirtySubmodules, true);
    assert.equal(initial.hasUnpushedSubmodules, true);

    backendFailed = true;
    await tree.refresh();
    const unavailable = provider.snapshot();
    assert.match(unavailable.errors.join('\n'), /backend status failed/);
    assert.equal(unavailable.repositories[0].statusUnavailable, true);
    assert.equal(unavailable.repositories[0].stashCount, 0);
    backendFailed = false;
    await tree.refresh();
    assert.equal(provider.snapshot().repositories[0].statusUnavailable, false);

    api.repositories.push(added);
    onOpen(added);
    t.mock.timers.tick(100);
    await settle();
    const rows = provider.snapshot().repositories;
    assert.equal(rows.length, 2);
    assert.equal(new Set(rows.map(node => node.name)).size, 2);
    assert.equal(rows.find(node => node.branch === 'feature').worktree, true);
    const beforeSortIds = new Map(rows.map(node => [node.branch, node.id]));
    tree.setSortOrder('alphabetical');
    t.mock.timers.tick(0);
    await settle();
    assert.deepEqual(provider.snapshot().repositories.map(node => node.branch), ['feature', 'main']);
    assert.equal(provider.snapshot().repositories.find(node => node.branch === 'main').id, beforeSortIds.get('main'));
    tree.setSortOrder('reverse');
    t.mock.timers.tick(0);
    await settle();
    assert.deepEqual(provider.snapshot().repositories.map(node => node.branch), ['main', 'feature']);
    sortFailure = true;
    tree.setSortOrder('dirty');
    t.mock.timers.tick(0);
    await settle();
    assert.match(provider.snapshot().errors.join('\n'), /sort service failed/);
    sortFailure = false;
    holdSort = true;
    tree.setSortOrder('alphabetical');
    t.mock.timers.tick(0);
    await settle();
    assert.equal(typeof releaseSort, 'function');
    tree.setSortOrder('reverse');
    t.mock.timers.tick(0);
    await settle();
    releaseSort();
    await settle();
    assert.deepEqual(provider.snapshot().repositories.map(node => node.branch), ['main', 'feature']);
    holdSort = false;
    tree.setSortOrder('workspace');

    tree.setVisible(true);
    failed = true;
    t.mock.timers.tick(5000);
    await settle();
    const failure = provider.snapshot();
    assert.match(failure.errors[0], /status failed/);
    assert.equal(failure.repositories.find(node => node.branch === 'main').statusUnavailable, false);
    assert.equal(countFiles(failure.repositories.find(node => node.branch === 'main')), 1);

    failed = false;
    addedFile = true;
    t.mock.timers.tick(5000);
    await settle();
    await provider.loadChanges(provider.snapshot().repositories.find(node => node.branch === 'main').id);
    const recovered = provider.snapshot();
    assert.deepEqual(recovered.errors, []);
    assert.equal(countFiles(recovered.repositories.find(node => node.branch === 'main')), 2);

    tree.setVisible(false);
    failed = true;
    t.mock.timers.tick(10000);
    await settle();
    assert.equal(tree.getChildren().some(node => node.type === 'error'), false);

    api.repositories = api.repositories.filter(item => item !== added);
    onClose(added);
    t.mock.timers.tick(50);
    assert.equal(tree.getChildren().filter(node => node.type === 'repo').length, 1);
    vscode.workspace.workspaceFolders = [{ uri: uri(otherWorkspace) }];
    onWorkspaceChange();
    await settle();
    assert.deepEqual(tree.getChildren().filter(node => node.type === 'repo').map(node => node.repo.rootUri.fsPath), [otherRoot]);
  } finally {
    provider.dispose();
    tree.dispose();
  }
});

test('workspace worktrees are opened automatically and expose their current file groups', async t => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-worktree-view-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const workspace = path.join(temporary, 'workspace');
  const root = path.join(workspace, 'parent');
  const linked = path.join(workspace, 'linked');
  const outside = path.join(temporary, 'outside');
  const common = path.join(root, '.git');
  for (const directory of [common, linked, outside]) fs.mkdirSync(directory, { recursive: true });
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const parent = { rootUri: uri(root), state: { onDidChange: event, HEAD: { name: 'main' } }, async status() {} };
  const child = { rootUri: uri(linked), kind: 'worktree', state: { onDidChange: event, HEAD: { name: 'topic' },
    indexChanges: [{ uri: uri(path.join(linked, 'staged.txt')), status: 0 }],
    workingTreeChanges: [{ uri: uri(path.join(linked, 'working.txt')), status: 5 }] }, async status() {} };
  let onOpen;
  let canOpen = false;
  let linkedExists = true;
  const opened = [];
  const api = { repositories: [parent],
    onDidOpenRepository(handler) { onOpen = handler; return { dispose() {} }; },
    onDidCloseRepository: event,
    async openRepository(target) {
      opened.push(target.fsPath);
      if (!canOpen) return undefined;
      this.repositories.push(child);
      onOpen(child);
      return child;
    }
  };
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event }
  };
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async beginChanges(value) { return changesPage(value === linked ? [
      change('index', 'staged.txt', 'index_modified'), change('working', 'working.txt', 'working_modified')
    ] : []); },
    async releaseChanges() {},
    async status(value, includeWorktrees = true) { return { common_dir: common, branch: value === root ? 'main' : 'topic',
      is_dirty: value === linked, ahead: 0, behind: 0, stashes: [],
      worktrees: includeWorktrees && value === root && linkedExists ? [{ path: linked, branch: 'topic', is_dirty: true, file_count: 2 },
        { path: outside, branch: 'outside' }] : [],
      has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false }; }
  };
  const load = Module._load;
  let RepositoryTree, GitpaneViewProvider;
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ RepositoryTree, GitpaneViewProvider } = require('../src/extension'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  try {
    await tree.refresh();
    let worktree = provider.snapshot().repositories.find(item => item.name === 'parent').worktrees[0];
    assert.equal(worktree.available, false);
    assert.deepEqual(worktree.groups, []);
    assert.deepEqual(opened, [linked]);
    canOpen = true;
    await tree.refresh();
    await tree.refresh();
    const snapshot = provider.snapshot();
    worktree = snapshot.repositories.find(item => item.name === 'parent').worktrees[0];
    assert.equal(worktree.available, true);
    await provider.loadChanges(worktree.id);
    worktree = provider.snapshot().repositories.find(item => item.name === 'parent').worktrees[0];
    assert.deepEqual(worktree.groups.map(group => [group.id, group.files.map(file => file.path)]), [
      ['index', ['staged.txt']], ['working', ['working.txt']]
    ]);
    const node = provider.changes.get(worktree.groups[1].files[0].id);
    assert.equal(provider.currentChange(node).repo, child);
    assert.equal(provider.currentWorktreeRepository(worktree.id), child);
    assert.equal(opened.includes(outside), false);
    linkedExists = false;
    await tree.refresh();
    assert.equal(provider.currentChange(node), undefined);
    assert.equal(provider.currentWorktreeRepository(worktree.id), undefined);
    assert.deepEqual(provider.snapshot().repositories.find(item => item.name === 'parent').worktrees, []);
  } finally {
    provider.dispose();
    tree.dispose();
  }
});

test('real linked worktrees appear once under an in-workspace anchor while explicit roots remain visible', async t => {
  const version = spawnSync('git', ['--version'], { encoding: 'utf8' });
  if (version.error?.code === 'ENOENT') return t.skip('Git is not installed');
  assert.ifError(version.error);
  assert.equal(version.status, 0, version.stderr);
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-worktree-topology-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const workspace = path.join(temporary, 'workspace');
  const root = path.join(workspace, 'main');
  const linkedRoot = path.join(workspace, 'worktrees');
  const first = path.join(linkedRoot, 'one');
  const second = path.join(linkedRoot, 'two');
  fs.mkdirSync(root, { recursive: true });
  fs.mkdirSync(linkedRoot, { recursive: true });
  const env = isolatedGitEnv();
  const git = (directory, ...args) => {
    const result = spawnSync('git', ['-C', directory, '-c', 'user.name=Gitpane Test',
      '-c', 'user.email=gitpane-test@example.invalid', ...args], { env, encoding: 'utf8' });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git(root, 'init', '-q');
  fs.writeFileSync(path.join(root, 'file.txt'), 'initial\n');
  git(root, 'add', '--', 'file.txt');
  git(root, 'commit', '-qm', 'initial');
  git(root, 'worktree', 'add', '-qb', 'topic-one', first);
  git(root, 'worktree', 'add', '-qb', 'topic-two', second);
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const makeRepo = (value, kind) => ({ rootUri: uri(value), kind,
    state: { onDidChange: event, HEAD: { name: git(value, 'branch', '--show-current') } }, async status() {} });
  const main = makeRepo(root, 'repository');
  const one = makeRepo(first, 'worktree');
  const two = makeRepo(second, 'worktree');
  const repos = [main, one, two];
  const common = fs.realpathSync(path.resolve(root, git(root, 'rev-parse', '--git-common-dir')));
  const statusRequests = [];
  let pauseDetails = false;
  let releaseDetails;
  const backend = { state: { status: 'ready' }, onDidChange: event,
    async status(value, includeWorktrees = true) {
      statusRequests.push([value, includeWorktrees]);
      if (includeWorktrees && pauseDetails) await new Promise(resolve => { releaseDetails = resolve; });
      const lines = git(value, 'worktree', 'list', '--porcelain').split('\n');
      const paths = lines.filter(line => line.startsWith('worktree ')).map(line => line.slice(9));
      return { common_dir: common, branch: git(value, 'branch', '--show-current'), is_dirty: false,
        ahead: 0, behind: 0, stashes: [], worktrees: includeWorktrees ?
          paths.filter(item => item !== root).map(item => ({ path: item, branch: path.basename(item) })) : [],
        has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false };
    }
  };
  const api = { repositories: repos, onDidOpenRepository: event, onDidCloseRepository: event };
  let onWorkspaceChange;
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }],
      onDidChangeWorkspaceFolders(handler) { onWorkspaceChange = handler; return { dispose() {} }; } } };
  const load = Module._load;
  let RepositoryTree, GitpaneViewProvider;
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ RepositoryTree, GitpaneViewProvider } = require('../src/extension'));
  } finally { Module._load = load; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  const provider = new GitpaneViewProvider(tree, '/extension');
  const changeFolders = async folders => {
    vscode.workspace.workspaceFolders = folders.map(folder => ({ uri: uri(folder) }));
    onWorkspaceChange();
    const deadline = Date.now() + 5000;
    while (tree.busy) {
      if (Date.now() > deadline) throw new Error('Workspace refresh timed out');
      await new Promise(resolve => setTimeout(resolve, 10));
    }
  };
  try {
    await tree.refresh();
    assert.deepEqual(statusRequests.filter(([, include]) => !include).map(([value]) => value).sort(), [root, first, second].sort());
    assert.deepEqual(statusRequests.filter(([, include]) => include).map(([value]) => value), [root]);
    let rows = provider.snapshot().repositories;
    assert.deepEqual(rows.map(row => row.name), ['main']);
    assert.deepEqual(rows[0].worktrees.map(row => row.name).sort(), ['one', 'two']);
    pauseDetails = true;
    const refresh = tree.refresh();
    const deadline = Date.now() + 5000;
    while (!releaseDetails) {
      if (Date.now() > deadline) throw new Error('Detailed status did not start');
      await new Promise(resolve => setImmediate(resolve));
    }
    rows = provider.snapshot().repositories;
    assert.deepEqual(rows.map(row => row.name), ['main']);
    assert.deepEqual(rows[0].worktrees.map(row => row.name).sort(), ['one', 'two']);
    let queuedFinished = false;
    const queuedRefresh = tree.refresh().then(() => { queuedFinished = true; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(queuedFinished, false, 'a refresh requested during an active sweep waits for its follow-up');
    const releaseFirst = releaseDetails;
    releaseDetails = undefined;
    releaseFirst();
    await refresh;
    while (!releaseDetails) {
      if (Date.now() > deadline) throw new Error('Follow-up refresh did not start');
      await new Promise(resolve => setImmediate(resolve));
    }
    assert.equal(queuedFinished, false, 'the queued refresh waits until its own sweep finishes');
    const thirdRefresh = tree.refresh();
    const releaseSecond = releaseDetails;
    releaseDetails = undefined;
    releaseSecond();
    let timeout;
    try {
      await Promise.race([queuedRefresh, new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error('Queued refresh waited for a later sweep')), 5000);
      })]);
    } finally { clearTimeout(timeout); }
    assert.equal(queuedFinished, true);
    while (!releaseDetails) {
      if (Date.now() > deadline) throw new Error('Third refresh did not start');
      await new Promise(resolve => setImmediate(resolve));
    }
    pauseDetails = false;
    releaseDetails();
    await thirdRefresh;
    await changeFolders([workspace, first]);
    rows = provider.snapshot().repositories;
    assert.deepEqual(rows.map(row => row.name).sort(), ['main', 'one']);
    assert.deepEqual(rows.find(row => row.name === 'main').worktrees.map(row => row.name), ['two']);
    assert.deepEqual(rows.find(row => row.name === 'one').worktrees, []);
    let explicitId = rows.find(row => row.name === 'one').id;
    const sessionTargets = tree.sessionTargets();
    assert.deepEqual(sessionTargets.map(target => target.path).sort(), [root, first, second].map(value => fs.realpathSync(value)).sort());
    assert.ok(sessionTargets.find(target => target.path === fs.realpathSync(second)).references.some(reference => reference.includes('worktree')));
    const removal = provider.resolveRepository(explicitId).worktreeRemoval;
    assert.equal(removal.repoPath, root);
    assert.equal(fs.realpathSync(removal.path), fs.realpathSync(first));
    assert.equal(removal.anchorToken, tree.repositoryToken(main));
    assert.equal(removal.targetToken, tree.repositoryToken(one));
    const anchorStatus = tree.backendStatuses.get(root);
    const registered = anchorStatus.worktrees;
    anchorStatus.worktrees = registered.filter(item => fs.realpathSync(item.path) !== fs.realpathSync(first));
    assert.equal(provider.resolveRepository(explicitId).worktreeRemoval, undefined, 'unregistered targets cannot be removed');
    anchorStatus.worktrees = registered;
    const childStatus = tree.backendStatuses.get(first);
    const unrelatedCommon = path.join(temporary, 'unrelated-common');
    fs.mkdirSync(unrelatedCommon);
    childStatus.common_dir = unrelatedCommon;
    assert.equal(provider.resolveRepository(explicitId).worktreeRemoval, undefined, 'unrelated repositories cannot be removed');
    childStatus.common_dir = common;
    const movedAnchor = path.join(workspace, 'moved-main');
    fs.renameSync(root, movedAnchor);
    assert.equal(provider.resolveRepository(explicitId).worktreeRemoval, undefined, 'a moved anchor cannot authorize removal');
    fs.renameSync(movedAnchor, root);
    const movedTarget = path.join(linkedRoot, 'moved-one');
    fs.renameSync(first, movedTarget);
    assert.equal(provider.resolveRepository(explicitId), undefined, 'a moved target cannot authorize removal');
    assert.ok(provider.snapshot().errors.some(message => message.includes(first) && /unavailable/i.test(message)));
    fs.renameSync(movedTarget, first);
    assert.ok(!provider.snapshot().errors.some(message => message.includes(first)), 'restoring the original folder clears its warning');
    fs.renameSync(first, movedTarget);
    fs.mkdirSync(first);
    assert.ok(provider.snapshot().errors.some(message => message.includes(first) && /remove and re-add/i.test(message)),
      'replaced folder remains unauthorized with an explicit instruction');
    fs.rmdirSync(first);
    fs.renameSync(movedTarget, first);
    assert.equal(provider.resolveRepository(explicitId), undefined, 'a removed row cannot reuse its old id');
    rows = provider.snapshot().repositories;
    explicitId = rows.find(row => row.name === 'one').id;
    const movedCommon = path.join(root, '.git-original');
    fs.renameSync(common, movedCommon);
    fs.mkdirSync(common);
    assert.notEqual(provider.resolveRepository(explicitId).worktreeRemoval.commonIdentity, removal.commonIdentity,
      'replaced common directory changes the authorization fingerprint');
    fs.rmdirSync(common);
    fs.renameSync(movedCommon, common);
    tree.unobserve(main);
    tree.observe(main);
    await tree.refresh();
    assert.notEqual(provider.resolveRepository(explicitId).worktreeRemoval.anchorToken, removal.anchorToken,
      'a reopened anchor changes the authorization fingerprint');
    tree.unobserve(one);
    tree.observe(one);
    assert.equal(provider.resolveRepository(explicitId), undefined, 'a reopened target cannot reuse an old id');
    await tree.refresh();
    await changeFolders([first]);
    rows = provider.snapshot().repositories;
    assert.equal(provider.resolveRepository(rows[0].id).worktreeRemoval, undefined, 'an anchor outside the workspace cannot authorize removal');
    await changeFolders([linkedRoot]);
    rows = provider.snapshot().repositories;
    assert.deepEqual(rows.map(row => row.name), ['one']);
    assert.deepEqual(rows[0].worktrees.map(row => row.name), ['two']);
    await changeFolders([linkedRoot, second]);
    rows = provider.snapshot().repositories;
    assert.deepEqual(rows.map(row => row.name).sort(), ['one', 'two']);
    assert.ok(rows.every(row => row.worktrees.length === 0));
    assert.equal(provider.resolveRepository(rows.find(row => row.name === 'two').id).worktreeRemoval.repoPath, first);
    const initiallyMissing = path.join(workspace, 'initially-missing');
    await changeFolders([workspace, initiallyMissing]);
    assert.ok(provider.snapshot().errors.some(message => message.includes(initiallyMissing) && /unavailable/i.test(message)));
    fs.mkdirSync(initiallyMissing);
    assert.ok(provider.snapshot().errors.some(message => message.includes(initiallyMissing) && /remove and re-add/i.test(message)));
    await changeFolders([workspace]);
    await changeFolders([workspace, initiallyMissing]);
    assert.ok(!provider.snapshot().errors.some(message => message.includes(initiallyMissing)));
    pauseDetails = true;
    releaseDetails = undefined;
    const activeRefresh = tree.refresh();
    const disposeDeadline = Date.now() + 5000;
    while (!releaseDetails) {
      if (Date.now() > disposeDeadline) throw new Error('Refresh before disposal did not start');
      await new Promise(resolve => setImmediate(resolve));
    }
    const pendingRefresh = tree.refresh();
    const releaseActive = releaseDetails;
    releaseDetails = undefined;
    releaseActive();
    await activeRefresh;
    while (!releaseDetails) {
      if (Date.now() > disposeDeadline) throw new Error('Refresh held during disposal did not start');
      await new Promise(resolve => setImmediate(resolve));
    }
    tree.dispose();
    let disposeTimeout;
    try {
      await Promise.race([pendingRefresh, new Promise((_, reject) => {
        disposeTimeout = setTimeout(() => reject(new Error('Disposed refresh remained pending')), 5000);
      })]);
    } finally { clearTimeout(disposeTimeout); }
  } finally {
    provider.dispose();
    tree.dispose();
  }
});

test('replaced repository and workspace symlinks cannot authorize stale actions', { skip: process.platform === 'win32' }, async t => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-scope-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const realWorkspace = path.join(temporary, 'workspace');
  const otherWorkspace = path.join(temporary, 'other-workspace');
  const realRepo = path.join(realWorkspace, 'real-repo');
  const outsideRepo = path.join(temporary, 'outside-repo');
  const workspace = path.join(temporary, 'workspace-link');
  const repoPath = path.join(workspace, 'repo');
  for (const dir of [realRepo, outsideRepo, otherWorkspace]) fs.mkdirSync(dir, { recursive: true });
  fs.mkdirSync(path.join(realRepo, '.git'));
  fs.symlinkSync(realWorkspace, workspace);
  fs.symlinkSync(realRepo, path.join(realWorkspace, 'repo'));
  fs.symlinkSync(realRepo, path.join(otherWorkspace, 'repo'));
  fs.writeFileSync(path.join(realRepo, 'file.txt'), 'inside');
  const uri = value => {
    const make = (scheme, query) => ({ scheme, fsPath: value, query,
      toString: () => scheme === 'file' && !query ? value : `${scheme}:${value}?${query}`,
      with: change => make(change.scheme ?? scheme, change.query ?? query) });
    return make('file', '');
  };
  const event = () => ({ dispose() {} });
  let statusCalls = 0;
  let backendCalls = 0;
  const apiChange = { uri: uri(path.join(repoPath, 'file.txt')), status: 7 };
  const repo = { rootUri: uri(repoPath), state: { HEAD: { name: 'main' }, onDidChange: event,
    workingTreeChanges: [apiChange] }, async status() { statusCalls++; } };
  let onOpen, onClose;
  const api = { repositories: [repo],
    onDidOpenRepository(handler) { onOpen = handler; return { dispose() {} }; },
    onDidCloseRepository(handler) { onClose = handler; return { dispose() {} }; },
    toGitUri: (value, ref) => ({ value, ref }) };
  const calls = [];
  const errors = [];
  const handlers = new Map();
  const graphRequests = [];
  const graphMessages = [];
  let receiveGraph, disposeGraph;
  const panel = { webview: { asWebviewUri: value => value, cspSource: 'test',
    postMessage: message => graphMessages.push(message),
    onDidReceiveMessage(handler) { receiveGraph = handler; } },
    onDidDispose(handler) { disposeGraph = handler; },
    dispose() { disposeGraph(); }, reveal() {} };
  let provider;
  let holdChanges = false;
  let releaseChanges;
  class Backend {
    constructor() { this.state = { status: 'ready' }; this.onDidChange = event; }
    async status() { backendCalls++; return { common_dir: path.join(realRepo, '.git'), branch: 'main',
      is_dirty: true, ahead: 0, behind: 0, stashes: [], worktrees: [],
      has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false }; }
    async graph(value) { graphRequests.push(value); return { rows: [], branches: [] }; }
    async graphRevision() { return { revision: 'a'.repeat(64) }; }
    async beginChanges() {
      if (holdChanges) await new Promise(resolve => { releaseChanges = resolve; });
      return changesPage([change('untracked', 'file.txt', 'untracked')]);
    }
    async verifyChanges() {
      const record = change('untracked', 'file.txt', 'untracked');
      return { page: changesPage([record]), change: record };
    }
    async releaseChanges() {}
    dispose() {}
  }
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') },
    ViewColumn: { Active: 1 },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event,
      registerTextDocumentContentProvider: event, fs: { stat: async () => ({}) } },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      registerWebviewViewProvider: (_name, value) => { provider = value; return { dispose() {} }; },
      createWebviewPanel: () => panel,
      showErrorMessage: message => errors.push(message) },
    commands: { registerCommand: (name, handler) => { handlers.set(name, handler); return { dispose() {} }; },
      executeCommand: async (...args) => calls.push(args) }
  };
  const load = Module._load;
  let activate;
  const context = { extensionUri: '/extension', extensionPath: '/extension', subscriptions: [] };
  delete require.cache[require.resolve('../src/extension')];
  delete require.cache[require.resolve('../src/repository-tree')];
  delete require.cache[require.resolve('../src/graph-provider')];
  delete require.cache[require.resolve('../src/graph/html')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './backend-manager') return { BackendManager: Backend };
      return load.call(this, name, ...args);
    };
    require('../src/graph/html');
    ({ activate } = require('../src/extension'));
    await activate(context);
  } finally { Module._load = load; }
  try {
    const snapshot = provider.snapshot();
    assert.equal(snapshot.repositories.length, 1);
    const previousId = snapshot.repositories[0].id;
    handlers.get('gitpane.openGraph')(previousId);
    await receiveGraph({ type: 'ready' });
    assert.deepEqual(graphRequests, [repoPath]);
    api.repositories = [];
    onClose(repo);
    const reopened = { ...repo, state: { ...repo.state } };
    api.repositories = [reopened];
    onOpen(reopened);
    handlers.get('gitpane.openGraph')(repo);
    handlers.get('gitpane.openGraph')(previousId);
    await receiveGraph({ type: 'refresh' });
    assert.deepEqual(graphRequests, [repoPath]);
    assert.match(graphMessages.at(-1).error, /no longer in the workspace/i);
    await provider.tree.refresh();
    const current = provider.snapshot().repositories[0];
    const repoId = current.id;
    assert.notEqual(repoId, previousId);
    handlers.get('gitpane.openGraph')(repoId);
    await receiveGraph({ type: 'ready' });
    assert.deepEqual(graphRequests, [repoPath, repoPath]);
    await provider.loadChanges(repoId);
    const fileId = provider.snapshot().repositories[0].groups[0].files[0].id;
    const node = provider.changes.get(fileId);
    await handlers.get('gitpane.openChange')(node);
    assert.equal(calls[0][0], 'vscode.diff');
    holdChanges = true;
    const refreshing = provider.loadChanges(repoId);
    while (!releaseChanges) await new Promise(resolve => setImmediate(resolve));
    const clickDuringRefresh = handlers.get('gitpane.openChange')(node);
    holdChanges = false;
    releaseChanges();
    await Promise.all([refreshing, clickDuringRefresh]);
    assert.equal(calls.length, 2);
    assert.equal(calls[1][0], 'vscode.diff');
    const beforeStatus = statusCalls;
    const beforeBackend = backendCalls;
    fs.unlinkSync(path.join(realWorkspace, 'repo'));
    fs.symlinkSync(outsideRepo, path.join(realWorkspace, 'repo'));
    assert.equal(provider.currentRepository(repoId), undefined);
    assert.equal(provider.currentChange(node), undefined);
    assert.deepEqual(provider.snapshot().repositories, []);
    await handlers.get('gitpane.openChange')(node);
    assert.equal(calls.length, 2);
    assert.match(errors.at(-1), /no longer in the workspace/i);
    handlers.get('gitpane.openGraph')(repo);
    assert.match(errors.at(-1), /no longer in the workspace/i);
    await provider.actions.reviewBranch(repoId);
    await provider.tree.refresh();
    assert.equal(statusCalls, beforeStatus);
    assert.equal(backendCalls, beforeBackend);

    fs.unlinkSync(path.join(realWorkspace, 'repo'));
    fs.symlinkSync(realRepo, path.join(realWorkspace, 'repo'));
    assert.equal(provider.tree.isVisible(repo), true);
    fs.unlinkSync(workspace);
    fs.symlinkSync(otherWorkspace, workspace);
    assert.equal(provider.tree.isVisible(repo), false);
    assert.deepEqual(provider.snapshot().repositories, []);
    await handlers.get('gitpane.openChange')(node);
    assert.equal(calls.length, 2);
    assert.match(errors.at(-1), /no longer in the workspace/i);
  } finally {
    for (const disposable of context.subscriptions) disposable.dispose();
    delete require.cache[require.resolve('../src/extension')];
    delete require.cache[require.resolve('../src/repository-tree')];
    delete require.cache[require.resolve('../src/graph-provider')];
    delete require.cache[require.resolve('../src/graph/html')];
  }
});

test('worktree graph opens for the linked repository and rejects a removed link', async t => {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-worktree-graph-'));
  t.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  const root = path.join(workspace, 'main');
  const linked = path.join(workspace, 'linked');
  const common = path.join(root, '.git');
  fs.mkdirSync(common, { recursive: true });
  fs.mkdirSync(linked);
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const makeRepo = (value, kind) => ({ rootUri: uri(value), kind,
    state: { HEAD: { name: 'main' }, onDidChange: event }, async status() {} });
  const parent = makeRepo(root, 'repository');
  const child = makeRepo(linked, 'worktree');
  const api = { repositories: [parent, child], onDidOpenRepository: event, onDidCloseRepository: event };
  const graphRequests = [];
  const graphMessages = [];
  const snapshots = [];
  let linkExists = true;
  let receiveSidebar, receiveGraph, disposeGraph, provider;
  class Backend {
    constructor() { this.state = { status: 'ready' }; this.onDidChange = event; }
    async status(value, includeWorktrees = true) { return { common_dir: common, branch: 'main', is_dirty: false,
      ahead: 0, behind: 0, stashes: [], worktrees: includeWorktrees && value === root && linkExists ?
        [{ path: linked, branch: 'topic' }] : [], has_dirty_submodules: false, has_unpushed_submodules: false,
      files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false }; }
    async graph(value) { graphRequests.push(value); return { rows: [], branches: [] }; }
    async graphRevision() { return { revision: 'a'.repeat(64) }; }
    dispose() {}
  }
  const webview = receive => ({ asWebviewUri: value => value, cspSource: 'test',
    onDidReceiveMessage: receive });
  const panel = { webview: { ...webview(handler => { receiveGraph = handler; }),
    postMessage: message => graphMessages.push(message) },
    onDidDispose(handler) { disposeGraph = handler; }, dispose() { disposeGraph(); }, reveal() {} };
  const view = { visible: true, webview: { ...webview(handler => { receiveSidebar = handler; return event(); }),
    postMessage: message => snapshots.push(message) }, onDidChangeVisibility: event, onDidDispose: event };
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event,
      registerTextDocumentContentProvider: event },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      registerWebviewViewProvider: (_name, value) => { provider = value; return event(); },
      createWebviewPanel: () => panel, showErrorMessage: message => { throw new Error(message); } },
    commands: { registerCommand: event, executeCommand: async () => {} }
  };
  const load = Module._load;
  const modules = ['../src/extension', '../src/repository-tree', '../src/graph-provider',
    '../src/graph/html', '../src/webview/html'];
  for (const name of modules) delete require.cache[require.resolve(name)];
  const context = { extensionUri: '/extension', extensionPath: '/extension', subscriptions: [] };
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './backend-manager') return { BackendManager: Backend };
      return load.call(this, name, ...args);
    };
    require('../src/graph/html');
    require('../src/webview/html');
    const { activate } = require('../src/extension');
    await activate(context);
  } finally { Module._load = load; }
  try {
    provider.resolveWebviewView(view);
    await receiveSidebar({ type: 'ready' });
    const worktree = snapshots.at(-1).repositories[0].worktrees[0];
    assert.equal(worktree.available, true);
    await receiveSidebar({ type: 'openWorktreeGraph', id: worktree.id });
    await receiveGraph({ type: 'ready' });
    assert.deepEqual(graphRequests, [linked]);

    linkExists = false;
    await provider.tree.refresh();
    await receiveGraph({ type: 'refresh' });
    assert.match(graphMessages.at(-1).error, /no longer in the workspace/i);
    await receiveSidebar({ type: 'openWorktreeGraph', id: worktree.id });
    assert.deepEqual(graphRequests, [linked]);
  } finally {
    for (const disposable of context.subscriptions) disposable.dispose();
    for (const name of modules) delete require.cache[require.resolve(name)];
  }
});
