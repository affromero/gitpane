'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');

test('webview shows workspace state and opens only currently visible changes', async () => {
  const calls = [];
  const vscode = {
    Uri: { joinPath: (...parts) => parts.join('/') },
    commands: { async executeCommand(...args) { calls.push(args); } }
  };
  const load = Module._load;
  let GitpaneViewProvider;
  try {
    Module._load = function (name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ GitpaneViewProvider } = require('../src/extension'));
  } finally {
    Module._load = load;
  }
  const root = path.resolve('workspace/repo');
  const uri = value => ({ fsPath: value, toString: () => value });
  const repo = { rootUri: uri(root), state: { HEAD: { name: 'topic', ahead: 2 }, indexChanges: [] } };
  let current = { uri: uri(path.join(root, 'file.txt')), status: 0 };
  let visible = true;
  let onChange;
  const tree = {
    git: { repositories: [repo] }, busy: false, failedStatuses: new Set(),
    onDidChangeTreeData(handler) { onChange = handler; return { dispose() {} }; },
    setVisible(value) { visible = value; },
    isVisible() { return visible; },
    groupsFor() { return current ? [{ id: 'index', label: 'Staged', changes: [current] }] : []; },
    getChildren(node) {
      if (!node) return [{ type: 'repo', repo }];
      if (node.type === 'repo') return this.groupsFor().map(group => ({ type: 'group', repo, group }));
      return node.group.changes.map(change => ({ type: 'change', repo, group: node.group.id, change }));
    },
    async refresh() { repo.state.HEAD.name = 'updated'; onChange(); }
  };
  let onMessage, onVisibility, onDispose;
  const snapshots = [];
  const view = {
    visible: true,
    webview: {
      postMessage(snapshot) { snapshots.push(snapshot); },
      onDidReceiveMessage(handler) { onMessage = handler; return { dispose() {} }; }
    },
    onDidChangeVisibility(handler) { onVisibility = handler; return { dispose() {} }; },
    onDidDispose(handler) { onDispose = handler; return { dispose() {} }; }
  };
  const provider = new GitpaneViewProvider(tree, '/extension');
  try {
    Module._load = function (name, ...args) {
      if (name === './webview/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    provider.resolveWebviewView(view);
  } finally {
    Module._load = load;
  }
  try {
    await onMessage({ type: 'ready' });
    const first = snapshots.at(-1);
    assert.equal(first.repositories[0].name, 'repo');
    assert.equal(first.repositories[0].branch, 'topic');
    assert.equal(first.repositories[0].ahead, 2);
    const file = first.repositories[0].groups[0].files[0];
    assert.equal(file.path, 'file.txt');
    assert.equal(file.status, 'M');
    const activeListener = onChange;
    const restarted = new GitpaneViewProvider(tree, '/extension');
    try {
      assert.notEqual(restarted.snapshot().repositories[0].id, first.repositories[0].id);
    } finally {
      restarted.dispose();
      visible = true;
      onChange = activeListener;
    }

    await onMessage({ type: 'refresh' });
    assert.equal(snapshots.at(-1).repositories[0].branch, 'updated');
    assert.equal(snapshots.at(-1).repositories[0].groups[0].files[0].id, file.id);
    await onMessage({ type: 'openChange', id: 'invalid', path: '/outside' });
    assert.deepEqual(calls, []);

    current = { uri: uri(path.join(root, 'file.txt')), status: 2 };
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls[0][0], 'gitpane.openChange');
    assert.equal(calls[0][1].change.status, 2);
    current = undefined;
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls.length, 1);
    onChange();
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls.length, 1);

    view.visible = false;
    onVisibility();
    assert.equal(visible, false);
    onDispose();
    assert.equal(visible, false);
  } finally {
    provider.dispose();
  }
});
