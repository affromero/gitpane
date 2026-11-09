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
  const roots = new Map([['one', { rootUri: { fsPath: '/one' } }], ['two', { rootUri: { fsPath: '/two' } }]]);
  const provider = { disposed: false, searchRequests: new Map(), pager: { release() {} },
    tree: { setVisible() {} }, scheduleChangesRefresh() {},
    snapshot: () => ({ type: 'snapshot', repositories: [{ id: 'one', name: 'one' }, { id: 'two', name: 'two' }] }),
    currentOwner: id => roots.get(id), currentRepository: id => roots.get(id),
    resolveRepositoryView(child) { this.view = child; child.webview.onDidReceiveMessage(message => {
      repositoryMessages.push(message);
      if (message.type === 'ready') { this.ready = true; this.workspaceViews.publish(); }
      if (message.type === 'selectOwner') this.workspaceViews.select(message.id);
    }); } };
  const graph = { resolveWebviewView(child) { this.view = child; child.webview.onDidReceiveMessage(message => graphMessages.push(message)); },
    clearSelection() {}, dispose() {} };
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
  assert.deepEqual(repositoryMessages, [{ type: 'ready' }]);
  assert.deepEqual(graphMessages, [{ type: 'ready' }]);
  deliver({ surface: 'repositories', message: { type: 'selectOwner', id: 'two' } });
  const latest = sent.filter(item => item.surface === 'changes' && item.message.type === 'snapshot').at(-1);
  assert.equal(latest.message.selectedOwner.id, 'two');
  assert.equal(latest.message.selectedOwnerId, 'two');
  assert.equal(repositoryMessages.length, 2);
  assert.equal(graphMessages.length, 1);
  workspace.dispose();
});
