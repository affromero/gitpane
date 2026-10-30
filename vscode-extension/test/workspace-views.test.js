'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const test = require('node:test');

class EventEmitter {
  constructor() {
    this.listeners = new Set();
    this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; };
  }
  fire(value) { for (const listener of [...this.listeners]) listener(value); }
  dispose() { this.listeners.clear(); }
}

const disposable = () => ({ dispose() {} });
const uri = (fsPath, scheme = 'file') => ({ fsPath, scheme, toString: () => `${scheme}:${fsPath}`,
  with: value => uri(fsPath, value.scheme || scheme) });
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

function view() {
  const visibility = new EventEmitter(), disposal = new EventEmitter();
  const messages = [];
  let receive;
  const result = { visible: true, title: '', messages,
    webview: { asWebviewUri: value => value, cspSource: 'test:', postMessage: message => messages.push(message),
      onDidReceiveMessage(handler) { receive = handler; return disposable(); } },
    onDidChangeVisibility: visibility.event, onDidDispose: disposal.event,
    receive: message => receive(message),
    setVisible(visible) { result.visible = visible; visibility.fire(); },
    destroy() { disposal.fire(); },
    snapshot: () => messages.filter(message => message.type === 'snapshot').at(-1) };
  return result;
}

async function settled(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  assert.ok(predicate(), 'The view did not reach its expected state');
}

async function fixture(t, { delayedDiscovery = false, unavailableWorktree = false, emptyWorkspace = false } = {}) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-stacked-'));
  const roots = ['api-service', 'web-app', 'api-auth'].map(name => path.join(temporary, name));
  for (const root of roots) {
    fs.mkdirSync(path.join(root, '.git'), { recursive: true });
    fs.writeFileSync(path.join(root, 'one.txt'), `${path.basename(root)}\n`);
  }
  const opened = new EventEmitter(), closed = new EventEmitter();
  const repositories = roots.map((root, index) => ({ rootUri: uri(root), kind: index === 2 ? 'worktree' : 'repository',
    state: { HEAD: { name: index === 2 ? 'feature/oauth' : 'main', commit: '1'.repeat(40) },
      onDidChange: disposable, indexChanges: [], workingTreeChanges: [], mergeChanges: [] }, async status() {} }));
  const api = { repositories: emptyWorkspace ? [] : delayedDiscovery ? [repositories[0], repositories[2]] :
    unavailableWorktree ? repositories.slice(0, 2) : [...repositories],
    onDidOpenRepository: opened.event, onDidCloseRepository: closed.event, async openRepository() {} };
  const state = new Map(delayedDiscovery ? [['gitpane.selectedRepository', roots[1]]] : []);
  const errors = [], diffs = [], panels = [], registrations = new Map(), commands = new Map();
  const commandGates = new Map(), graphGates = new Map(), graphRequests = [];
  let snapshotNumber = 0;
  let themeError = 'Theme service is unavailable in this workspace';
  const record = { group: 'untracked', kind: 'untracked', path: 'one.txt', original_path: null,
    is_submodule: false, conflict_stages: null };
  const page = () => ({ snapshot: `changes-${++snapshotNumber}`, revision: 'd'.repeat(64), cursor: 0,
    previous_cursor: null, next_cursor: null, changes: [{ ...record }] });
  class Backend {
    constructor() { this.state = { status: 'ready' }; this.onDidChange = disposable; this.generation = 1; }
    async status(root, worktrees = true) {
      const index = roots.indexOf(root);
      return { repo_identity: ['a', 'b', 'c'][index].repeat(64), common_dir: path.join(index === 2 ? roots[0] : root, '.git'),
        branch: index === 2 ? 'feature/oauth' : 'main', head_oid: '1'.repeat(40), is_dirty: true, ahead: 0, behind: 0,
        stashes: [], worktrees: index === 0 && worktrees ?
          [{ path: roots[2], branch: 'feature/oauth', repo_identity: 'c'.repeat(64) }] : [],
        has_dirty_submodules: false, has_unpushed_submodules: false, files: [], watched_errors: [],
        file_count_lower_bound: 1, files_truncated: false };
    }
    async uiConfig() { return { expand_worktrees: true }; }
    async theme() { throw new Error(themeError); }
    async graph(root) {
      graphRequests.push(root);
      const gate = graphGates.get(root);
      if (gate) { graphGates.delete(root); gate.started.resolve(); await gate.release.promise; }
      return { rows: [], branches: [] };
    }
    async graphRevision() { return { revision: 'e'.repeat(64) }; }
    async beginChanges() { return page(); }
    async releaseChanges() {}
    async verifyChanges() { return { page: page(), change: { ...record } }; }
    dispose() {}
  }
  const vscode = { EventEmitter,
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(temporary) }], onDidChangeWorkspaceFolders: disposable,
      onDidChangeConfiguration: disposable,
      getConfiguration: () => ({ get: (key, fallback) => key === 'showSessionLiveness' ? false : fallback }),
      registerTextDocumentContentProvider: disposable, registerFileSystemProvider: disposable },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      registerWebviewViewProvider(name, provider) { registrations.set(name, provider); return disposable(); },
      createWebviewPanel() {
        const panel = view();
        panel.reveal = () => {};
        panel.dispose = () => panel.destroy();
        panels.push(panel);
        return panel;
      },
      showErrorMessage: message => errors.push(message), showQuickPick: async choices => choices[0] },
    commands: { registerCommand(name, handler) { commands.set(name, handler); return disposable(); },
      async executeCommand(name, ...args) {
        if (name === 'vscode.diff') { diffs.push(args); return; }
        const gate = commandGates.get(name);
        if (gate) { commandGates.delete(name); gate.started.resolve(); await gate.release.promise; }
        return commands.get(name)?.(...args);
      } }
  };
  const modules = ['../src/extension', '../src/repository-tree', '../src/graph-provider', '../src/graph/html', '../src/webview/html'];
  for (const name of modules) delete require.cache[require.resolve(name)];
  const load = Module._load;
  const context = { extensionUri: '/extension', subscriptions: [], extension: { id: 'affromero.gitpane-vscode' },
    workspaceState: { get: key => state.get(key), async update(key, value) { state.set(key, value); } } };
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './backend-manager') return { BackendManager: Backend };
      return load.call(this, name, ...args);
    };
    require('../src/webview/html');
    require('../src/graph/html');
    await require('../src/extension').activate(context);
  } finally { Module._load = load; }
  t.after(() => {
    for (const subscription of context.subscriptions) subscription.dispose();
    for (const name of modules) delete require.cache[require.resolve(name)];
    fs.rmSync(temporary, { recursive: true, force: true });
  });
  const resolve = async kind => {
    const result = view();
    registrations.get(`gitpane.${kind}`).resolveWebviewView(result);
    await result.receive({ type: 'ready' });
    return result;
  };
  return { roots, api, repositories, state, errors, diffs, panels, vscode, commands, resolve, commandGates, graphGates, graphRequests,
    failTheme(message) { themeError = message; },
    resolveGraphWithoutReady() {
      const graph = view();
      registrations.get('gitpane.commitGraph').resolveWebviewView(graph);
      return graph;
    },
    async discoverWorktree() { api.repositories.push(repositories[2]); opened.fire(repositories[2]); await commands.get('gitpane.refresh')(); },
    async discoverSecond() { api.repositories.push(repositories[1]); opened.fire(repositories[1]); await commands.get('gitpane.refresh')(); } };
}

function message(view, type, id, extra = {}) {
  const snapshot = view.snapshot();
  return { type, id, ownerId: snapshot.selectedOwnerId, selectionEpoch: snapshot.selectionEpoch, ...extra };
}

test('stacked lower views restore selection during discovery without resolving repositories', async t => {
  const f = await fixture(t, { delayedDiscovery: true });
  const changes = await f.resolve('changes');
  const graph = await f.resolve('commitGraph');
  assert.equal(changes.snapshot().selectedOwner.name, 'api-service');
  assert.equal(f.state.get('gitpane.selectedRepository'), f.roots[1]);
  await f.discoverSecond();
  await settled(() => changes.snapshot().selectedOwner.name === 'web-app');
  await settled(() => graph.messages.some(item => item.type === 'graph' && item.repoName === 'web-app' && !item.loading));
  assert.equal(f.state.get('gitpane.selectedRepository'), f.roots[1]);
  assert.equal(f.panels.length, 0, 'Selecting a repository must not create an editor tab');
  assert.ok(graph.messages.some(item => item.type === 'themeError' && item.error.includes('Theme service is unavailable')));
});

test('an explicit repo choice replaces the saved preference during discovery', async t => {
  const f = await fixture(t, { delayedDiscovery: true });
  const repos = await f.resolve('repositories');
  const changes = await f.resolve('changes');
  await repos.receive({ type: 'selectOwner', id: changes.snapshot().selectedOwnerId });
  await f.discoverSecond();
  assert.equal(changes.snapshot().selectedOwner.name, 'api-service');
  assert.equal(f.state.get('gitpane.selectedRepository'), f.roots[0]);
});

test('Changes can retry a failed theme without a repository selection or overview', async t => {
  const f = await fixture(t, { emptyWorkspace: true });
  const changes = await f.resolve('changes');
  await settled(() => changes.snapshot().errors.some(error => error.includes('Theme service is unavailable')));
  assert.equal(changes.snapshot().selectedOwnerId, undefined);
  f.failTheme('Theme service failed again');
  await changes.receive(message(changes, 'refresh'));
  assert.ok(changes.snapshot().errors.some(error => error.includes('Theme service failed again')));
});

test('repository and worktree selection update lower sections through collapse and recreation', async t => {
  const f = await fixture(t);
  const repos = await f.resolve('repositories');
  const changes = await f.resolve('changes');
  const graph = await f.resolve('commitGraph');
  const parent = repos.snapshot().repositories.find(item => item.name === 'api-service');
  const worktree = parent.worktrees[0];
  await repos.receive({ type: 'selectOwner', id: worktree.id });
  await changes.receive(message(changes, 'loadChanges', worktree.id));
  assert.equal(changes.snapshot().selectedOwner.name, 'api-auth');
  assert.equal(changes.snapshot().selectedKind, 'worktree');
  assert.equal(changes.snapshot().selectedOwner.groups[0].files[0].path, 'one.txt');
  await settled(() => graph.messages.some(item => item.type === 'graph' && item.repoName === 'api-auth' && !item.loading));
  repos.setVisible(false);
  await changes.receive(message(changes, 'refresh', worktree.id));
  assert.equal(changes.snapshot().selectedOwner.name, 'api-auth');
  graph.setVisible(false);
  changes.destroy();
  const reopened = await f.resolve('changes');
  assert.equal(reopened.snapshot().selectedOwner.name, 'api-auth');
  assert.equal(reopened.snapshot().selectedOwner.changesPage.state, 'idle');
  await reopened.receive(message(reopened, 'loadChanges', worktree.id));
  await reopened.receive(message(reopened, 'openChange', reopened.snapshot().selectedOwner.groups[0].files[0].id));
  assert.equal(f.diffs.at(-1)[1].fsPath, path.join(f.roots[2], 'one.txt'));
  graph.setVisible(true);
  const firstParent = { type: 'firstParent', value: true };
  await graph.receive(firstParent);
  await settled(() => graph.messages.some(item => item.type === 'graph' && item.firstParent === true && !item.loading));
  assert.equal(f.panels.length, 0);
  await f.commands.get('gitpane.openGraph')(parent.id);
  await f.panels[0].receive({ type: 'ready' });
  assert.equal(f.panels[0].messages.filter(item => item.type === 'graph').at(-1).firstParent, false);
  assert.equal(graph.messages.filter(item => item.type === 'graph').at(-1).firstParent, true);
});

test('changing repositories cancels a dispatched Changes diff without stale errors', { timeout: 5000 }, async t => {
  const f = await fixture(t);
  const repos = await f.resolve('repositories');
  const changes = await f.resolve('changes');
  const first = changes.snapshot().selectedOwnerId;
  await changes.receive(message(changes, 'loadChanges', first));
  const file = changes.snapshot().selectedOwner.groups[0].files[0];
  const gate = { started: deferred(), release: deferred() };
  f.commandGates.set('gitpane.openChange', gate);
  const opening = changes.receive(message(changes, 'openChange', file.id));
  await gate.started.promise;
  const second = repos.snapshot().repositories.find(item => item.name === 'web-app');
  await repos.receive({ type: 'selectOwner', id: second.id });
  gate.release.resolve();
  await opening;
  assert.deepEqual(f.diffs, []);
  assert.deepEqual(f.errors, []);
  assert.equal(changes.snapshot().selectedOwner.name, 'web-app');
});

test('an unavailable worktree remains selected when its repository opens', async t => {
  const f = await fixture(t, { unavailableWorktree: true });
  const repos = await f.resolve('repositories');
  const changes = await f.resolve('changes');
  const graph = await f.resolve('commitGraph');
  const worktree = repos.snapshot().repositories.find(item => item.name === 'api-service').worktrees[0];
  assert.equal(worktree.available, false);
  await repos.receive({ type: 'selectOwner', id: worktree.id });
  assert.equal(changes.snapshot().selectedOwner.available, false);
  await f.discoverWorktree();
  await settled(() => changes.snapshot().selectedOwner.name === 'api-auth' && changes.snapshot().selectedOwner.available);
  await settled(() => graph.messages.some(item => item.type === 'graph' && item.repoName === 'api-auth' && !item.loading));
});

test('the graph waits for readiness and ignores an older repo response after a rapid switch', { timeout: 5000 }, async t => {
  const f = await fixture(t);
  const repos = await f.resolve('repositories');
  const graph = f.resolveGraphWithoutReady();
  const first = repos.snapshot().repositories.find(item => item.name === 'api-service');
  const second = repos.snapshot().repositories.find(item => item.name === 'web-app');
  const gate = { started: deferred(), release: deferred() };
  f.graphGates.set(f.roots[1], gate);
  await repos.receive({ type: 'selectOwner', id: second.id });
  assert.deepEqual(f.graphRequests, []);
  const ready = graph.receive({ type: 'ready' });
  await gate.started.promise;
  await repos.receive({ type: 'selectOwner', id: first.id });
  await settled(() => graph.messages.some(item => item.type === 'graph' && item.repoName === 'api-service' && !item.loading));
  gate.release.resolve();
  await ready;
  const rendered = graph.messages.filter(item => item.type === 'graph' && !item.loading && item.repoName);
  assert.equal(rendered.at(-1).repoName, 'api-service');
  assert.ok(!rendered.some(item => item.repoName === 'web-app'));
});

test('hidden or recreated Changes views reject old selection messages', async t => {
  const f = await fixture(t);
  const repos = await f.resolve('repositories');
  const changes = await f.resolve('changes');
  const first = changes.snapshot().selectedOwnerId;
  await changes.receive(message(changes, 'loadChanges', first));
  const file = changes.snapshot().selectedOwner.groups[0].files[0];
  const old = message(changes, 'openChange', file.id);
  changes.setVisible(false);
  await changes.receive(old);
  changes.setVisible(true);
  const second = repos.snapshot().repositories.find(item => item.name === 'web-app');
  await repos.receive({ type: 'selectOwner', id: second.id });
  await repos.receive({ type: 'selectOwner', id: first });
  await changes.receive(old);
  assert.deepEqual(f.diffs, []);
  assert.deepEqual(f.errors, []);
});
