'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');

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
    workspace: {
      workspaceFolders: [{ uri: uri(workspace) }],
      onDidChangeWorkspaceFolders(handler) { onWorkspaceChange = handler; return { dispose() {} }; }
    }
  };
  const backend = {
    state: { status: 'ready' },
    onDidChange: event,
    async status(repoPath) {
      if (backendFailed) throw new Error('backend status failed');
      return {
        branch: repoPath === root ? 'main' : 'feature', is_dirty: repoPath === root,
        ahead: 2, behind: 1, stashes: [{ index: 0 }],
        worktrees: [{ path: addedRoot }, { path: path.join(otherWorkspace, 'external') }],
        has_dirty_submodules: true, has_unpushed_submodules: true
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
    assert.equal(failure.repositories.find(node => node.branch === 'main').statusUnavailable, true);

    failed = false;
    addedFile = true;
    t.mock.timers.tick(5000);
    await settle();
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
  fs.symlinkSync(realWorkspace, workspace);
  fs.symlinkSync(realRepo, path.join(realWorkspace, 'repo'));
  fs.symlinkSync(realRepo, path.join(otherWorkspace, 'repo'));
  fs.writeFileSync(path.join(realRepo, 'file.txt'), 'inside');
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value,
    with: change => ({ fsPath: value, ...change }) });
  const event = () => ({ dispose() {} });
  let statusCalls = 0;
  let backendCalls = 0;
  const change = { uri: uri(path.join(repoPath, 'file.txt')), status: 7 };
  const repo = { rootUri: uri(repoPath), state: { HEAD: { name: 'main' }, onDidChange: event,
    workingTreeChanges: [change] }, async status() { statusCalls++; } };
  const api = { repositories: [repo], onDidOpenRepository: event, onDidCloseRepository: event,
    toGitUri: (value, ref) => ({ value, ref }) };
  const calls = [];
  const errors = [];
  const handlers = new Map();
  let provider;
  class Backend {
    constructor() { this.state = { status: 'ready' }; this.onDidChange = event; }
    async status() { backendCalls++; return { common_dir: path.join(realRepo, '.git'), branch: 'main',
      is_dirty: true, ahead: 0, behind: 0, stashes: [], worktrees: [],
      has_dirty_submodules: false, has_unpushed_submodules: false }; }
    dispose() {}
  }
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(workspace) }], onDidChangeWorkspaceFolders: event,
      registerTextDocumentContentProvider: event, fs: { stat: async () => ({}) } },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      registerWebviewViewProvider: (_name, value) => { provider = value; return { dispose() {} }; },
      showErrorMessage: message => errors.push(message) },
    commands: { registerCommand: (name, handler) => { handlers.set(name, handler); return { dispose() {} }; },
      executeCommand: async (...args) => calls.push(args) }
  };
  const load = Module._load;
  let activate;
  const context = { extensionUri: '/extension', extensionPath: '/extension', subscriptions: [] };
  delete require.cache[require.resolve('../src/extension')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './backend-manager') return { BackendManager: Backend };
      return load.call(this, name, ...args);
    };
    ({ activate } = require('../src/extension'));
    await activate(context);
  } finally { Module._load = load; }
  try {
    const snapshot = provider.snapshot();
    assert.equal(snapshot.repositories.length, 1);
    const repoId = snapshot.repositories[0].id;
    const fileId = snapshot.repositories[0].groups[0].files[0].id;
    const node = provider.changes.get(fileId);
    await handlers.get('gitpane.openChange')(node);
    assert.equal(calls[0][0], 'vscode.diff');
    const beforeStatus = statusCalls;
    const beforeBackend = backendCalls;
    fs.unlinkSync(path.join(realWorkspace, 'repo'));
    fs.symlinkSync(outsideRepo, path.join(realWorkspace, 'repo'));
    assert.equal(provider.currentRepository(repoId), undefined);
    assert.equal(provider.currentChange(node), undefined);
    assert.deepEqual(provider.snapshot().repositories, []);
    await handlers.get('gitpane.openChange')(node);
    assert.equal(calls.length, 1);
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
    assert.equal(calls.length, 1);
    assert.match(errors.at(-1), /no longer in the workspace/i);
  } finally {
    for (const disposable of context.subscriptions) disposable.dispose();
    delete require.cache[require.resolve('../src/extension')];
  }
});
