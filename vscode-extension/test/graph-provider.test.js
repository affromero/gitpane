'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');

test('commit files open native diffs against the first parent and reject stale actions', async () => {
  const messages = [];
  const commands = [];
  const errors = [];
  let receive, onDispose, onTreeChange;
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
    window: { createWebviewPanel: () => panel, showErrorMessage: message => { errors.push(message); } },
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
  const tree = {
    git: { repositories: [repo] },
    isVisible: () => visible,
    onDidChangeTreeData(handler) { onTreeChange = handler; return { dispose() {} }; }
  };
  const rootOid = 'a'.repeat(40);
  const mergeOid = 'b'.repeat(40);
  const firstParent = 'c'.repeat(40);
  const backend = {
    async graph() { return { branches: [], rows: [
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
    provider.open(repo);
  } finally { Module._load = load; }

  try {
    await receive({ type: 'ready' });
    assert.equal(messages.at(-1).rows.length, 2);
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
    assert.equal(messages.at(-1).type, 'graph');
    assert.equal(messages.at(-1).rows.length, 2);

    visible = false;
    onTreeChange();
    await receive({ type: 'openCommitFile', id: mergeFile.id });
    assert.equal(commands.length, 3);
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
      onDidChangeTreeData() { return { dispose() {} }; } };
    const backend = {
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
      onDidChangeTreeData() { return { dispose() {} }; } };
    const backend = {
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
