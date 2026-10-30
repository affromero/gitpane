'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const { createHash } = require('node:crypto');
const { BackendManager } = require('../src/backend-manager');

function change(group, file, kind) {
  return { group, path: file, kind, original_path: null, conflict_stages: null, is_submodule: false };
}

function changesPage(changes) {
  return { snapshot: 'fixture-snapshot', revision: createHash('sha256').update(JSON.stringify(changes)).digest('hex'), cursor: 0,
    previous_cursor: null, next_cursor: null, changes };
}

class EventEmitter {
  constructor() {
    this.listeners = new Set();
    this.event = handler => {
      this.listeners.add(handler);
      return { dispose: () => this.listeners.delete(handler) };
    };
  }
  fire(value) { for (const handler of this.listeners) handler(value); }
  dispose() { this.listeners.clear(); }
}

async function workspaceFixture(t) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-folder-errors-'));
  const workspace = path.join(temporary, 'workspace');
  const repoPath = path.join(workspace, 'repo');
  const ordinary = path.join(temporary, 'ordinary');
  fs.mkdirSync(path.join(repoPath, '.git'), { recursive: true });
  fs.mkdirSync(ordinary);
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const makeRepo = value => ({ rootUri: uri(value), state: { HEAD: { name: 'main' },
    onDidChange: new EventEmitter().event }, async status() {} });
  const repo = makeRepo(repoPath);
  const opened = new EventEmitter(), closed = new EventEmitter(), folders = new EventEmitter();
  const api = { repositories: [repo], onDidOpenRepository: opened.event, onDidCloseRepository: closed.event };
  const vscode = { EventEmitter, workspace: {
    workspaceFolders: [workspace, ordinary].map(value => ({ uri: uri(value) })),
    onDidChangeWorkspaceFolders: folders.event, onDidChangeConfiguration: new EventEmitter().event,
    getConfiguration: () => ({ get: (name, fallback) => name === 'backendPath' ? '' : fallback })
  } };
  const requests = [];
  const backend = new BackendManager({ extensionPath: temporary }, { vscode, createClient() {
    let roots = [], sources = [], tokens = [];
    return {
      async initialize(values, identities, paths) {
        roots = values; sources = paths; tokens = identities || roots.map(() => 'a'.repeat(64));
      },
      async workspaceRootIdentities() {
        return { roots: roots.map((value, index) => ({ path: value, source: sources[index], identity: tokens[index] })) };
      },
      async status(value) {
        requests.push({ repo: value, roots: [...roots] });
        return { common_dir: path.join(value, '.git'), branch: 'main', is_dirty: false, ahead: 0, behind: 0,
          stashes: [], worktrees: [], has_dirty_submodules: false, has_unpushed_submodules: false,
          files: [], watched_errors: [], file_count_lower_bound: 0, files_truncated: false };
      },
      dispose() {}
    };
  } });
  let tree;
  t.after(() => { tree?.dispose(); backend.dispose(); fs.rmSync(temporary, { recursive: true, force: true }); });
  await backend.ready();
  const load = Module._load;
  delete require.cache[require.resolve('../src/repository-tree')];
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
    const { RepositoryTree } = require('../src/repository-tree');
    tree = new RepositoryTree(api, { appendLine() {} }, backend);
  } finally { Module._load = load; }
  await tree.refresh();
  return { tree, backend, workspace, ordinary, temporary, repo, requests,
    errors: () => tree.getChildren().filter(node => node.type === 'error').map(node => node.message),
    open(value) { const item = makeRepo(value); api.repositories.push(item); opened.fire(item); return item; },
    close(item) { api.repositories.splice(api.repositories.indexOf(item), 1); closed.fire(item); },
    async setFolders(values) {
      vscode.workspace.workspaceFolders = values.map(value => ({ uri: uri(value) }));
      folders.fire({});
      await backend.ready();
      await tree.refresh();
    }
  };
}

test('ordinary folder deletion and recreation leave repository status available without warnings', async t => {
  const f = await workspaceFixture(t);
  assert.deepEqual(f.errors(), []);
  // Keep Linux from reusing the deleted directory's inode for its replacement.
  const heldDirectory = process.platform === 'linux' ? fs.openSync(f.ordinary, 'r') : undefined;
  if (heldDirectory !== undefined) t.after(() => fs.closeSync(heldDirectory));
  fs.rmSync(f.ordinary, { recursive: true });
  await f.tree.refresh();
  assert.deepEqual(f.errors(), []);
  fs.mkdirSync(f.ordinary);
  await f.tree.refresh();
  assert.deepEqual(f.errors(), []);
  const canonical = fs.realpathSync.native(f.workspace);
  const expectedRoot = process.platform === 'win32' ? canonical.toLowerCase() : canonical;
  assert.deepEqual((await f.backend.mutationSnapshot()).roots, [expectedRoot]);
  assert.equal((await f.backend.status(f.repo.rootUri.fsPath)).branch, 'main');
  assert.deepEqual(f.requests.at(-1), { repo: f.repo.rootUri.fsPath, roots: [expectedRoot] });
  assert.deepEqual(f.tree.getChildren().filter(node => node.type === 'repo').map(node => node.repo), [f.repo]);
  const missing = path.join(f.temporary, 'initially-missing');
  await f.setFolders([f.workspace, f.ordinary, missing]);
  assert.deepEqual(f.errors(), []);
  fs.mkdirSync(missing);
  await f.tree.refresh();
  assert.deepEqual(f.errors(), []);
  assert.equal(f.tree.isPathInWorkspace(missing), false, 'hiding an irrelevant warning does not authorize the replacement');
});

test('newly detected Git repositories show a replaced folder warning and closing them clears it', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = await workspaceFixture(t);
  const saved = path.join(f.temporary, 'saved');
  fs.renameSync(f.ordinary, saved);
  fs.mkdirSync(f.ordinary);
  t.mock.timers.tick(100);
  const updates = [];
  f.tree.onDidChangeTreeData(() => updates.push(f.errors()));
  fs.mkdirSync(path.join(f.ordinary, '.git'));
  const added = f.open(f.ordinary);
  t.mock.timers.tick(50);
  assert.ok(updates.at(-1)?.some(message => message.includes(f.ordinary) && /remove and re-add/i.test(message)));
  assert.equal(f.tree.isVisible(added), false);
  assert.ok(!f.tree.getChildren().some(node => node.type === 'repo' && node.repo === added));
  f.close(added);
  t.mock.timers.tick(50);
  assert.deepEqual(updates.at(-1), []);
});

test('retargeted workspace aliases warn for nested Git repositories reported by canonical path',
  { skip: process.platform === 'win32' }, async t => {
    const f = await workspaceFixture(t);
    const saved = path.join(f.temporary, 'saved-workspace');
    fs.renameSync(f.workspace, saved);
    fs.symlinkSync(saved, f.workspace);
    f.close(f.repo);
    const added = f.open(fs.realpathSync(path.join(f.workspace, 'repo')));
    assert.ok(f.errors().some(message => message.includes(f.workspace) && /remove and re-add/i.test(message)));
    assert.equal(f.tree.isVisible(added), false);
  });

test('workspace replacement warnings match Windows Git paths with different casing',
  { skip: process.platform !== 'win32' }, async t => {
    const f = await workspaceFixture(t);
    fs.renameSync(f.ordinary, path.join(f.temporary, 'saved'));
    fs.mkdirSync(path.join(f.ordinary, '.git'), { recursive: true });
    const added = f.open(f.ordinary.toUpperCase());
    assert.ok(f.errors().some(message => message.includes(f.ordinary) && /remove and re-add/i.test(message)));
    assert.equal(f.tree.isVisible(added), false);
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
  let apiRepositories = [repo];
  const api = { get repositories() { return apiRepositories.map(item => ({ ...item })); },
    set repositories(value) { apiRepositories = value; },
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
  let holdVerification = false;
  let releaseVerification;
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
      if (holdVerification) await new Promise(resolve => { releaseVerification = resolve; });
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
      onDidChangeConfiguration: event, getConfiguration: () => ({ get: (name, fallback) => fallback }),
      registerTextDocumentContentProvider: event, registerFileSystemProvider: event, fs: { stat: async () => ({}) } },
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
    let repoId = current.id;
    assert.notEqual(repoId, previousId);
    handlers.get('gitpane.openGraph')(repoId);
    await receiveGraph({ type: 'ready' });
    assert.deepEqual(graphRequests, [repoPath, repoPath]);
    await provider.loadChanges(repoId);
    const fileId = provider.snapshot().repositories[0].groups[0].files[0].id;
    let node = provider.changes.get(fileId);
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
    holdVerification = true;
    const staleClick = handlers.get('gitpane.openChange')(node);
    while (!releaseVerification) await new Promise(resolve => setImmediate(resolve));
    api.repositories = [];
    onClose(reopened);
    api.repositories = [{ ...reopened }];
    onOpen(reopened);
    holdVerification = false;
    releaseVerification();
    await staleClick;
    assert.equal(calls.length, 2, 'a reopened repository cannot receive the pending diff');
    await provider.tree.refresh();
    repoId = provider.snapshot().repositories[0].id;
    await provider.loadChanges(repoId);
    node = provider.changes.get(provider.snapshot().repositories[0].groups[0].files[0].id);
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
