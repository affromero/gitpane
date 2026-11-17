'use strict';

const assert = require('node:assert/strict');
const Module = require('node:module');
const { test } = require('node:test');
const { WorkspaceViews } = require('../src/workspace-views');

test('unified view routes each section and shares the selected repository', async () => {
  const listeners = new Set(), sent = [], repositoryMessages = [], graphMessages = [];
  const event = listener => { listeners.add(listener); return { dispose: () => listeners.delete(listener) }; };
  const view = { visible: true, onDidDispose: () => ({ dispose() {} }), onDidChangeVisibility: () => ({ dispose() {} }),
    webview: { cspSource: 'test:', asWebviewUri: value => value, onDidReceiveMessage: event,
      postMessage: message => sent.push(message) } };
  const roots = new Map([['one', { rootUri: { fsPath: '/one/lib' } }], ['two', { rootUri: { fsPath: '/two/lib' } }]]);
  const provider = { disposed: false, searchRequests: new Map(), pager: { release() {} },
    tree: { setVisible() {} }, scheduleChangesRefresh() {},
    snapshot: () => ({ type: 'snapshot', repositories: [{ id: 'one', name: 'lib', branch: '(no branch)' },
      { id: 'two', name: 'lib', branch: 'HEAD' }] }),
    currentOwner: id => roots.get(id), currentRepository: id => roots.get(id),
    resolveRepositoryView(child) { this.view = child; child.webview.onDidReceiveMessage(message => {
      repositoryMessages.push(message);
      if (message.type === 'ready') { this.ready = true; this.workspaceViews.publish(); }
      if (message.type === 'selectOwner') this.workspaceViews.select(message.id);
    }); } };
  const graph = { rows: [], resolveWebviewView(child) { this.view = child; child.webview.onDidReceiveMessage(message => graphMessages.push(message)); },
    clearSelection() { this.rows = []; sent.push({ cleared: true }); }, dispose() {} };
  const vscode = { Uri: { joinPath: (...parts) => parts.join('/') }, window: { showErrorMessage() {} } };
  const workspace = new WorkspaceViews(vscode, { extensionUri: '/extension' }, provider, graph, () => {});
  const load = Module._load;
  try {
    Module._load = function(name, ...args) {
      if (name === './unified/html' || name === './webview/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    workspace.resolveUnifiedView(view);
  } finally { Module._load = load; }
  const deliver = envelope => { for (const listener of [...listeners]) listener(envelope); };
  deliver({ surface: 'repositories', message: { type: 'ready' } });
  deliver({ surface: 'changes', message: { type: 'ready' } });
  deliver({ surface: 'graph', message: { type: 'ready' } });
  assert.equal(sent.filter(item => item.surface === 'graph' && item.message.type === 'sectionTitle').at(-1).message.title,
    'Git Graph: /one/lib · (no branch)');
  assert.deepEqual(repositoryMessages, [{ type: 'ready' }]);
  assert.deepEqual(graphMessages, [{ type: 'ready' }]);
  const previous = sent.length;
  graph.rows = ['commit from one'];
  deliver({ surface: 'repositories', message: { type: 'selectOwner', id: 'two' } });
  assert.deepEqual(graph.rows, [], 'the previous graph is cleared even when the new graph does not open');
  const transition = sent.slice(previous);
  const cleared = transition.findIndex(item => item.cleared);
  const heading = transition.findIndex(item => item.surface === 'graph' && item.message.type === 'sectionTitle');
  assert.ok(cleared >= 0 && heading > cleared);
  const latest = sent.filter(item => item.surface === 'changes' && item.message.type === 'snapshot').at(-1);
  assert.equal(latest.message.selectedOwner.id, 'two');
  assert.equal(latest.message.selectedOwnerId, 'two');
  assert.equal(sent.filter(item => item.surface === 'changes' && item.message.type === 'sectionTitle').at(-1).message.title,
    'Changes: /two/lib · detached HEAD');
  assert.equal(sent.filter(item => item.surface === 'graph' && item.message.type === 'sectionTitle').at(-1).message.title,
    'Git Graph: /two/lib · detached HEAD');
  assert.equal(repositoryMessages.length, 2);
  assert.equal(graphMessages.length, 1);
  sent.length = 0;
  deliver({ surface: 'graph', message: { type: 'ready' } });
  assert.equal(sent.filter(item => item.surface === 'graph' && item.message.type === 'sectionTitle').at(-1).message.title,
    'Git Graph: /two/lib · detached HEAD', 'a reloaded graph receives the current owner heading');
  workspace.dispose();
});
