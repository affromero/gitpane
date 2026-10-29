'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');

test('commit files open native diffs against the first parent and reject stale actions', async () => {
  const messages = [];
  const commands = [];
  const errors = [];
  let receive, onDispose, onTreeChange, panelOptions;
  const panel = {
    webview: {
      postMessage(message) { messages.push(message); },
      onDidReceiveMessage(handler) { receive = handler; }
    },
    onDidDispose(handler) { onDispose = handler; },
    dispose() { onDispose(); },
    reveal() {}
  };
  const uri = value => ({ fsPath: value, toString: () => value,
    with: change => ({ fsPath: value, ...change }) });
  const vscode = {
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') },
    ViewColumn: { Active: 1 },
    window: { createWebviewPanel: (_type, _title, _column, options) => { panelOptions = options; return panel; }, showErrorMessage: message => { errors.push(message); } },
    commands: { async executeCommand(...args) { commands.push(args); } }
  };
  const load = Module._load;
  let GraphProvider;
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ GraphProvider } = require('../src/graph-provider'));
  } finally { Module._load = load; }

  const root = path.resolve('workspace/repo');
  const repo = { rootUri: uri(root) };
  let visible = true;
  let linked = true;
  let incarnation = 1;
  const tree = {
    git: { repositories: [repo] },
    isVisible: () => visible,
    repositoryToken: () => incarnation,
    isCurrentRepository: (_, token) => visible && token === incarnation,
    onDidChangeTreeData(handler) { onTreeChange = handler; return { dispose() {} }; }
  };
  const rootOid = 'a'.repeat(40);
  const mergeOid = 'b'.repeat(40);
  const firstParent = 'c'.repeat(40);
  let labelMaxLen;
  const backend = {
    async graphRevision() { return { revision: 'a'.repeat(64) }; },
    async graph() { return { branches: [], label_max_len: labelMaxLen, rows: [
      { oid: rootOid, parent_oids: [] },
      { oid: mergeOid, parent_oids: [firstParent, 'd'.repeat(40)] }
    ] }; },
    async commitFiles(_repo, oid) {
      return { files: [
        { status: oid === rootOid ? 'A' : 'M', path: 'src/file with spaces.rs' },
        { status: 'M', path: 'vendor/module', is_submodule: true },
        { status: 'R', path: 'new/location.rs', old_path: 'old/location.rs' }
      ] };
    }
  };
  const git = { toGitUri: (value, ref) => ({ fsPath: value.fsPath, ref }) };
  const provider = new GraphProvider(tree, backend, git, '/extension');
  try {
    Module._load = function(name, ...args) {
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    provider.open(repo, () => linked);
    assert.deepEqual(panelOptions.localResourceRoots, ['/extension/src']);
  } finally { Module._load = load; }

  try {
    await receive({ type: 'ready' });
    assert.equal(messages.at(-1).rows.length, 2);
    assert.equal(messages.at(-1).labelMaxLen, 24);
    labelMaxLen = 0;
    await receive({ type: 'refresh' });
    assert.equal(messages.at(-1).labelMaxLen, 0);
    labelMaxLen = 7;
    await receive({ type: 'refresh' });
    assert.equal(messages.at(-1).labelMaxLen, 7);
    labelMaxLen = -1;
    await receive({ type: 'refresh' });
    assert.match(messages.at(-1).error, /invalid graph label length/i);
    labelMaxLen = undefined;
    await receive({ type: 'refresh' });
    await receive({ type: 'selectCommit', oid: rootOid });
    const rootFile = messages.at(-1).files[0];
    await receive({ type: 'openCommitFile', id: rootFile.id });
    assert.equal(commands[0][0], 'vscode.diff');
    assert.equal(commands[0][1].scheme, 'gitpane-empty');
    assert.equal(commands[0][2].ref, rootOid);

    await receive({ type: 'selectCommit', oid: mergeOid });
    const mergeFile = messages.at(-1).files[0];
    await receive({ type: 'openCommitFile', id: rootFile.id });
    assert.equal(commands.length, 1);
    await receive({ type: 'openCommitFile', id: mergeFile.id });
    assert.equal(commands[1][0], 'vscode.diff');
    assert.equal(commands[1][1].ref, firstParent);
    assert.equal(commands[1][2].ref, mergeOid);
    assert.equal(commands[1][2].fsPath, path.join(root, 'src/file with spaces.rs'));
    await receive({ type: 'openCommitFile', id: messages.at(-1).files[1].id });
    assert.equal(commands.length, 2);
    assert.match(errors[0], /submodule/i);
    await receive({ type: 'openCommitFile', id: messages.at(-1).files[2].id });
    assert.equal(commands[2][1].fsPath, path.join(root, 'old/location.rs'));
    assert.equal(commands[2][1].ref, firstParent);
    assert.equal(commands[2][2].fsPath, path.join(root, 'new/location.rs'));
    assert.equal(commands[2][2].ref, mergeOid);

    await receive({ type: 'refresh' });
    assert.equal(messages.at(-1).type, 'commitFiles');
    assert.equal(messages.at(-1).oid, mergeOid);

    linked = false;
    await receive({ type: 'openCommitFile', id: mergeFile.id });
    assert.equal(commands.length, 3);
    provider.open(repo);
    await receive({ type: 'ready' });
    await receive({ type: 'selectCommit', oid: mergeOid });
    const reopenedFile = messages.at(-1).files[0];
    await receive({ type: 'openCommitFile', id: reopenedFile.id });
    assert.equal(commands.length, 4);

    incarnation++;
    await receive({ type: 'openCommitFile', id: reopenedFile.id });
    assert.equal(commands.length, 4);
    provider.open(repo);
    await receive({ type: 'ready' });
    await receive({ type: 'selectCommit', oid: mergeOid });
    const newFile = messages.at(-1).files[0];
    await receive({ type: 'openCommitFile', id: newFile.id });
    assert.equal(commands.length, 5);

    visible = false;
    onTreeChange();
    await receive({ type: 'openCommitFile', id: mergeFile.id });
    assert.equal(commands.length, 5);
    assert.match(messages.at(-1).error, /workspace/);
  } finally { provider.dispose(); }
});

test('graph filters retain catalog choices, distinguish all and none, and reject stale picks', async () => {
  const messages = [];
  const calls = [];
  const queue = [];
  const panel = {
    webview: { postMessage: message => messages.push(message), onDidReceiveMessage() {} },
    onDidDispose(handler) { this.onDispose = handler; },
    dispose() { this.onDispose(); }, reveal() {}
  };
  const uri = value => ({ fsPath: value, toString: () => value });
  const vscode = {
    Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    window: {
      createWebviewPanel: () => panel,
      async showQuickPick(choices, options) {
        const choose = queue.shift();
        assert.ok(choose, 'unexpected filter picker');
        return choose(choices, options);
      },
      async showInformationMessage() {}, async showErrorMessage(message) { throw new Error(message); }
    }
  };
  const load = Module._load;
  let GraphProvider;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    ({ GraphProvider } = require('../src/graph-provider'));
    const root = path.resolve('workspace/repo');
    const repo = { rootUri: uri(root) };
    let visible = true;
    const tree = { git: { repositories: [repo] }, isVisible: () => visible,
      repositoryToken: () => 1, isCurrentRepository: (_, token) => visible && token === 1,
      onDidChangeTreeData() { return { dispose() {} }; } };
    const backend = {
      async graphRevision() { return { revision: 'a'.repeat(64) }; },
      async graph(_root, firstParent, filters) {
        calls.push({ firstParent, filters: structuredClone(filters) });
        const rows = [{ oid: 'a'.repeat(40), author: 'Ada' }, { oid: 'b'.repeat(40), author: 'Bob' }];
        return { branches: ['main', 'feature/a'], rows: filters.branches?.length === 0 ? [] :
          rows.filter(row => filters.authors === null || filters.authors.includes(row.author)) };
      }
    };
    const provider = new GraphProvider(tree, backend, {}, '/extension');
    provider.open(repo);
    try {
      await provider.receive({ type: 'ready' });
      assert.deepEqual(calls.at(-1).filters, { branches: null, authors: null,
        refs: { local: true, remote: true, tags: true, stashes: true } });
      queue.push(choices => choices.find(item => item.category === 'branches'));
      queue.push((choices, options) => { assert.equal(options.canPickMany, true); assert.equal(choices.length, 2); return []; });
      await provider.receive({ type: 'openFilters' });
      assert.deepEqual(calls.at(-1).filters.branches, []);
      assert.equal(messages.at(-1).rows.length, 0);

      queue.push(choices => choices.find(item => item.category === 'authors'));
      queue.push(choices => {
        assert.deepEqual(choices.map(item => item.value), ['Ada', 'Bob']);
        return choices.filter(item => item.value === 'Bob');
      });
      await provider.receive({ type: 'openFilters' });
      assert.deepEqual(calls.at(-1).filters.authors, ['Bob']);
      assert.deepEqual(calls.at(-1).filters.branches, []);

      queue.push(choices => choices.find(item => item.category === 'refs'));
      queue.push(choices => choices.filter(item => ['local', 'tags'].includes(item.value)));
      await provider.receive({ type: 'openFilters' });
      assert.deepEqual(calls.at(-1).filters.refs, { local: true, remote: false, tags: true, stashes: false });

      queue.push(choices => choices.find(item => item.category === 'branches'));
      queue.push(choices => choices);
      await provider.receive({ type: 'openFilters' });
      assert.equal(calls.at(-1).filters.branches, null);

      queue.push(choices => choices.find(item => item.category === 'reset'));
      await provider.receive({ type: 'openFilters' });
      assert.deepEqual(calls.at(-1).filters, { branches: null, authors: null,
        refs: { local: true, remote: true, tags: true, stashes: true } });

      const beforeCancel = calls.length;
      queue.push(() => undefined);
      await provider.receive({ type: 'openFilters' });
      assert.equal(calls.length, beforeCancel);

      let finish;
      queue.push(choices => choices.find(item => item.category === 'authors'));
      queue.push(choices => new Promise(resolve => { finish = () => resolve([choices[0]]); }));
      const pending = provider.receive({ type: 'openFilters' });
      await new Promise(resolve => setImmediate(resolve));
      visible = false;
      finish();
      await pending;
      assert.equal(calls.length, beforeCancel);
    } finally { provider.dispose(); }
  } finally {
    Module._load = load;
    delete require.cache[require.resolve('../src/graph-provider')];
  }
});

test('graph collapses only visible side branches and does not open placeholder commits', async () => {
  const messages = [];
  const calls = [];
  const oid = 'a'.repeat(40);
  const trunk = 'b'.repeat(40);
  const panel = {
    webview: { postMessage: message => messages.push(message), onDidReceiveMessage() {} },
    onDidDispose(handler) { this.onDispose = handler; },
    dispose() { this.onDispose(); }, reveal() {}
  };
  const uri = value => ({ fsPath: value, toString: () => value });
  const vscode = {
    Uri: { joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    window: { createWebviewPanel: () => panel, showErrorMessage() {} }
  };
  const load = Module._load;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    const { GraphProvider } = require('../src/graph-provider');
    const root = path.resolve('workspace/repo');
    const repo = { rootUri: uri(root) };
    let visible = true;
    let detailCalls = 0;
    const tree = { git: { repositories: [repo] }, isVisible: () => visible,
      repositoryToken: () => 1, isCurrentRepository: (_, token) => visible && token === 1,
      onDidChangeTreeData() { return { dispose() {} }; } };
    const backend = {
      async graphRevision() { return { revision: 'a'.repeat(64) }; },
      async graph(_repo, firstParent, _filters, collapsed) {
        calls.push({ firstParent, collapsed: [...collapsed] });
        return { branches: [], rows: [
          { oid: trunk, segment_id: null, collapsed: null },
          { oid, segment_id: oid, collapsed: collapsed.includes(oid) ? { id: oid, count: 2 } : null }
        ] };
      },
      async commitFiles() { detailCalls++; return { files: [] }; }
    };
    const provider = new GraphProvider(tree, backend, {}, '/extension');
    provider.open(repo);
    try {
      await provider.receive({ type: 'ready' });
      assert.deepEqual(calls.at(-1).collapsed, []);
      await provider.receive({ type: 'toggleCollapse', id: 'c'.repeat(40) });
      assert.equal(calls.length, 1);
      await provider.receive({ type: 'toggleCollapse', id: oid });
      assert.deepEqual(calls.at(-1).collapsed, [oid]);
      assert.equal(messages.at(-1).collapsedCount, 1);
      assert.equal(messages.at(-1).rows[1].collapsed.count, 2);
      await provider.receive({ type: 'selectCommit', oid });
      assert.equal(detailCalls, 0);
      await provider.receive({ type: 'toggleCollapse', id: oid });
      assert.deepEqual(calls.at(-1).collapsed, []);
      await provider.receive({ type: 'selectCommit', oid });
      assert.equal(detailCalls, 1);
      await provider.receive({ type: 'toggleCollapse', id: oid });
      await provider.receive({ type: 'expandAll' });
      assert.deepEqual(calls.at(-1).collapsed, []);
      await provider.receive({ type: 'toggleCollapse', id: oid });
      await provider.receive({ type: 'firstParent', value: true });
      assert.deepEqual(calls.at(-1), { firstParent: true, collapsed: [] });
      const count = calls.length;
      visible = false;
      await provider.receive({ type: 'toggleCollapse', id: oid });
      assert.equal(calls.length, count);
    } finally { provider.dispose(); }
  } finally {
    Module._load = load;
    delete require.cache[require.resolve('../src/graph-provider')];
  }
});

test('visible graphs refresh when refs change, reject late polls, and recover from errors', async () => {
  const messages = [];
  const calls = [];
  const commands = [];
  let viewState;
  const panel = {
    visible: true,
    webview: { postMessage: message => messages.push(message), onDidReceiveMessage() {} },
    onDidChangeViewState(handler) { viewState = handler; },
    onDidDispose(handler) { this.onDispose = handler; },
    dispose() { this.onDispose(); }, reveal() {}
  };
  const uri = value => ({ fsPath: value, toString: () => value, with: change => ({ fsPath: value, ...change }) });
  const vscode = { Uri: { joinPath: (...parts) => parts.join('/'), file: uri }, ViewColumn: { Active: 1 },
    window: { createWebviewPanel: () => panel }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const load = Module._load;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    const { GraphProvider } = require('../src/graph-provider');
    const repo = { rootUri: uri(path.resolve('workspace/repo')), state: { HEAD: { commit: 'initial' } } };
    let incarnation = 1;
    let active = true;
    let onTreeChange;
    const tree = { git: { repositories: [repo] }, isVisible: () => true,
      repositoryToken: () => incarnation, isCurrentRepository: (_, token) => active && token === incarnation,
      onDidChangeTreeData(handler) { onTreeChange = handler; return { dispose() {} }; } };
    const oid = '1'.repeat(40);
    let revision = 'a'.repeat(64);
    let revisionError;
    let pendingRevision;
    let pendingGraph;
    let changeDuringGraph;
    const backend = {
      async graphRevision() {
        if (revisionError) throw new Error(revisionError);
        if (pendingRevision) return pendingRevision;
        return { revision };
      },
      async graph(_repo, firstParent, filters, collapsed) {
        calls.push({ firstParent, filters: structuredClone(filters), collapsed: [...collapsed] });
        if (changeDuringGraph) { revision = changeDuringGraph; changeDuringGraph = undefined; }
        if (pendingGraph) return pendingGraph;
        return { branches: ['main'], rows: [{ oid, parent_oids: [], segment_id: oid,
          collapsed: collapsed.includes(oid) ? { id: oid, count: 1 } : null }] };
      },
      async commitFiles() { return { files: [{ path: 'changed.txt', status: 'A', is_submodule: false }] }; }
    };
    const provider = new GraphProvider(tree, backend, { toGitUri: (value, ref) => ({ ...value, ref }) }, '/extension');
    provider.open(repo);
    try {
      await provider.receive({ type: 'ready' });
      await provider.receive({ type: 'selectCommit', oid });
      const oldFile = messages.at(-1).files[0].id;
      provider.filters.refs.tags = false;
      revision = 'b'.repeat(64);
      await provider.pollRevision();
      assert.equal(calls.length, 2);
      assert.equal(calls.at(-1).filters.refs.tags, false);
      assert.equal(messages.at(-1).type, 'commitFiles');
      assert.equal(messages.at(-1).oid, oid);
      await provider.receive({ type: 'openCommitFile', id: oldFile });
      assert.equal(commands.length, 0, 'old file actions are invalidated by refresh');
      await provider.pollRevision();
      assert.equal(calls.length, 2, 'unchanged revision does not reload graph');

      revisionError = 'fingerprint unavailable';
      await provider.pollRevision();
      assert.match(messages.at(-1).error, /fingerprint unavailable/);
      revisionError = undefined;
      await provider.pollRevision();
      assert.equal(calls.length, 3);
      assert.equal(messages.at(-1).oid, oid, 'selection survives recovery');

      let finish;
      pendingRevision = new Promise(resolve => { finish = resolve; });
      const pending = provider.pollRevision();
      await Promise.resolve();
      await provider.pollRevision();
      panel.visible = false;
      viewState();
      assert.equal(provider.revision, undefined, 'reveal must reload invalidated file actions even without ref changes');
      const messageCountWhileHidden = messages.length;
      repo.state.HEAD.commit = 'updated';
      onTreeChange();
      assert.equal(messages.length, messageCountWhileHidden, 'HEAD updates do not load hidden graphs');
      finish({ revision: 'c'.repeat(64) });
      await pending;
      pendingRevision = undefined;
      assert.equal(calls.length, 3, 'hidden panel ignores a late revision');
      assert.equal(provider.pollTimer, undefined);
      panel.visible = true;
      revision = 'c'.repeat(64);
      viewState();
      await provider.pollRevision();
      assert.equal(calls.length, 4, 'showing the panel checks immediately');

      revision = 'd'.repeat(64);
      changeDuringGraph = 'e'.repeat(64);
      await provider.pollRevision();
      assert.equal(calls.length, 5);
      assert.equal(provider.revision, undefined, 'mid-load ref changes are never accepted as current');
      await provider.pollRevision();
      assert.equal(calls.length, 6);
      assert.equal(provider.revision, 'e'.repeat(64));

      for (const next of ['f', 'a', 'b']) {
        changeDuringGraph = next.repeat(64);
        provider.revision = undefined;
        await provider.pollRevision();
      }
      assert.equal(calls.length, 9);
      await new Promise(resolve => setTimeout(resolve, 50));
      assert.equal(calls.length, 9, 'persistent changes stop immediate retries');
      await provider.pollRevision();
      assert.equal(calls.length, 10);
      assert.equal(provider.revision, 'b'.repeat(64), 'graph recovers when repository activity settles');

      pendingRevision = new Promise(resolve => { finish = resolve; });
      const stale = provider.pollRevision();
      await Promise.resolve();
      incarnation++;
      finish({ revision: 'd'.repeat(64) });
      await stale;
      assert.equal(calls.length, 10, 'a reopened repository rejects a prior incarnation response');

      pendingRevision = undefined;
      provider.open(repo);
      await provider.receive({ type: 'ready' });
      let fail;
      pendingRevision = new Promise((_, reject) => { fail = reject; });
      const staleLoad = provider.receive({ type: 'refresh' });
      const beforeFailure = messages.length;
      incarnation++;
      fail(new Error('stale backend error'));
      await staleLoad;
      assert.equal(messages.length, beforeFailure, 'a stale load error cannot update a replaced repository');

      pendingRevision = undefined;
      provider.open(repo);
      await provider.receive({ type: 'ready' });
      let finishGraph;
      pendingGraph = new Promise(resolve => { finishGraph = resolve; });
      const interruptedLoad = provider.receive({ type: 'refresh' });
      await new Promise(resolve => setImmediate(resolve));
      const callsDuringLoad = calls.length;
      for (const head of ['one', 'two', 'three']) {
        repo.state.HEAD.commit = head;
        onTreeChange();
      }
      await new Promise(resolve => setImmediate(resolve));
      assert.equal(calls.length, callsDuringLoad, 'repeated HEAD events do not overlap a graph load');
      active = false;
      onTreeChange();
      assert.match(messages.at(-1).error, /no longer in the workspace/i);
      active = true;
      pendingGraph = undefined;
      onTreeChange();
      await provider.pollRevision();
      assert.equal(messages.at(-1).loading, false, 'graph reloads after the workspace guard recovers');
      const messageCount = messages.length;
      finishGraph({ branches: [], rows: [] });
      await interruptedLoad;
      assert.equal(messages.length, messageCount, 'an interrupted graph cannot overwrite the recovered graph');
    } finally { provider.dispose(); }
  } finally {
    Module._load = load;
    delete require.cache[require.resolve('../src/graph-provider')];
  }
});

test('backend and ancestor validation errors do not cause rapid graph retries', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-graph-backoff-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const messages = [], commands = [];
  const event = () => ({ dispose() {} });
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = fn => { this.listeners.add(fn); return { dispose: () => this.listeners.delete(fn) }; }; }
    fire(value) { for (const fn of this.listeners) fn(value); }
    dispose() { this.listeners.clear(); }
  }
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value,
    with: change => ({ fsPath: value, ...change }) });
  const panel = { visible: true, webview: { postMessage: message => messages.push(message), onDidReceiveMessage: event },
    onDidChangeViewState: event, onDidDispose(handler) { this.onDispose = handler; },
    dispose() { this.onDispose(); }, reveal() {} };
  const vscode = { EventEmitter, Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    workspace: { workspaceFolders: [{ uri: uri(root) }], onDidChangeWorkspaceFolders: event,
      onDidChangeConfiguration: event, getConfiguration: () => ({ get: () => '' }) },
    window: { createWebviewPanel: () => panel }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const load = Module._load;
  for (const name of ['../src/repository-tree', '../src/graph-provider']) delete require.cache[require.resolve(name)];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    const { RepositoryTree } = require('../src/repository-tree');
    const { BackendManager } = require('../src/backend-manager');
    const { GraphProvider } = require('../src/graph-provider');
    const repo = { rootUri: uri(root), state: { HEAD: { commit: 'head' }, onDidChange: event } };
    const git = { repositories: [repo], onDidOpenRepository: event, onDidCloseRepository: event };
    let failing = true;
    let revisions = 0, guardCalls = 0, guardFailure = false;
    const oid = 'a'.repeat(40);
    const manager = new BackendManager({ extensionPath: root }, { vscode, realpath: async value => value,
      createClient: () => ({ async initialize() {}, dispose() {},
        async graphRevision() { revisions++; if (failing) throw new Error('offline'); return { revision: 'a'.repeat(64) }; },
        async graph() { return { branches: [], rows: [{ oid, parent_oids: [] }] }; },
        async commitFiles() { return { files: [{ path: 'file.txt', status: 'A', is_submodule: false }] }; },
        async branchReview() { guardCalls++; if (guardFailure) throw new Error('ancestor unavailable'); return {}; } }) });
    await manager.ready();
    const tree = new RepositoryTree(git, { appendLine() {} }, manager);
    const provider = new GraphProvider(tree, manager, git, '/extension');
    try {
      provider.open(repo);
      await provider.receive({ type: 'ready' });
      assert.match(messages.at(-1).error, /offline/);
      assert.equal(revisions, 1);
      await new Promise(resolve => setTimeout(resolve, 250));
      assert.equal(revisions, 1, 'delayed backend and tree notifications must respect retry backoff');
      failing = false;
      await provider.pollRevision();
      assert.equal(messages.at(-1).loading, false);
      assert.equal(messages.at(-1).error, undefined);
      assert.equal(manager.state.status, 'ready');

      provider.open(repo, () => true, async () => { await manager.branchReview(root); return true; });
      await provider.receive({ type: 'ready' });
      await provider.receive({ type: 'selectCommit', oid });
      const file = messages.at(-1).files[0];
      guardFailure = true;
      await provider.receive({ type: 'openCommitFile', id: file.id });
      assert.match(messages.at(-1).error, /ancestor unavailable/);
      assert.equal(commands.length, 0, 'failed validation prevents file actions');
      const attempts = guardCalls;
      await new Promise(resolve => setTimeout(resolve, 250));
      assert.equal(guardCalls, attempts, 'delayed error notifications must not trigger rapid validation retries');
      guardFailure = false;
      await provider.pollRevision();
      assert.equal(messages.at(-1).error, undefined);
      assert.equal(manager.state.status, 'ready');
    } finally { provider.dispose(); tree.dispose(); manager.dispose(); }
  } finally {
    Module._load = load;
    for (const name of ['../src/repository-tree', '../src/graph-provider']) delete require.cache[require.resolve(name)];
  }
});

test('visible submodule graphs revalidate ancestor guards without Explorer events', async () => {
  const messages = [], commands = [];
  let onViewState;
  const panel = { visible: true, webview: { postMessage: value => messages.push(value), onDidReceiveMessage() {} },
    onDidChangeViewState(handler) { onViewState = handler; }, onDidDispose(handler) { this.onDispose = handler; },
    dispose() { this.onDispose(); }, reveal() {} };
  const uri = value => ({ fsPath: value, toString: () => value, with: change => ({ fsPath: value, ...change }) });
  const vscode = { Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    window: { createWebviewPanel: () => panel }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const load = Module._load;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    const { GraphProvider } = require('../src/graph-provider');
    const repo = { rootUri: uri(path.resolve('workspace/child')) };
    const tree = { git: { repositories: [repo] }, isVisible: () => true, repositoryToken: () => 1,
      isCurrentRepository: (_, token) => token === 1, onDidChangeTreeData() { return { dispose() {} }; } };
    const oid = 'a'.repeat(40);
    const backend = { async graphRevision() { return { revision: 'a'.repeat(64) }; },
      async graph() { return { branches: [], rows: [{ oid, parent_oids: [] }] }; },
      async commitFiles() { return { files: [{ path: 'file.txt', status: 'A', is_submodule: false }] }; } };
    let valid = true, validations = 0, pause = false, finishValidation;
    const provider = new GraphProvider(tree, backend, { toGitUri: (value, ref) => ({ ...value, ref }) }, '/extension');
    provider.open(repo, () => true, async () => {
      validations++;
      if (pause) await new Promise(resolve => { finishValidation = resolve; });
      return valid;
    });
    try {
      await provider.receive({ type: 'ready' });
      pause = true;
      const before = validations;
      const poll = provider.pollRevision();
      const selection = provider.receive({ type: 'selectCommit', oid });
      await Promise.resolve();
      assert.equal(validations, before + 1, 'polling and user actions share one ancestor validation');
      pause = false;
      finishValidation();
      await Promise.all([poll, selection]);
      await provider.receive({ type: 'selectCommit', oid });
      const file = messages.at(-1).files[0];
      valid = false;
      await provider.pollRevision();
      assert.match(messages.at(-1).error, /parent submodule changed/i);
      await provider.receive({ type: 'openCommitFile', id: file.id });
      assert.equal(commands.length, 0, 'a stale ancestor cannot open a commit diff');
      panel.visible = false;
      onViewState();
      const previous = validations;
      await provider.pollRevision();
      assert.equal(validations, previous, 'hidden graphs do not poll ancestor guards');
    } finally { provider.dispose(); }
  } finally {
    Module._load = load;
    delete require.cache[require.resolve('../src/graph-provider')];
  }
});
