'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const { ComparisonPanels } = require('../src/comparison-panels');

test('comparison graphs keep independent repository contexts and lifetimes', () => {
  const panels = [];
  const uri = fsPath => ({ fsPath, toString: () => fsPath });
  const vscode = { Uri: { joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    window: { createWebviewPanel(_type, title) {
      const listeners = [];
      let closed = false;
      const panel = { title, visible: true, webview: { postMessage() {}, onDidReceiveMessage: () => ({ dispose() {} }) },
        onDidDispose(handler) { listeners.push(handler); return { dispose() {} }; },
        dispose() { if (!closed) { closed = true; for (const listener of listeners) listener(); } }, reveal() {} };
      panels.push(panel);
      return panel;
    } } };
  const load = Module._load;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    const { GraphProvider } = require('../src/graph-provider');
    const firstRepo = { rootUri: uri('/one') }, secondRepo = { rootUri: uri('/two') };
    const tree = { git: { repositories: [firstRepo, secondRepo] }, repositoryToken: () => 1,
      isCurrentRepository: () => true, isVisible: () => true, onDidChangeTreeData: () => ({ dispose() {} }) };
    const comparisons = new ComparisonPanels(GraphProvider, tree, {}, {}, '/extension');
    comparisons.configureView({ refreshInterval: 5000 });
    const first = comparisons.open(firstRepo, () => true);
    const second = comparisons.open(secondRepo, () => true);
    assert.notEqual(panels[0], panels[1]);
    assert.equal(first.currentRepo(), firstRepo);
    assert.equal(second.currentRepo(), secondRepo);
    panels[0].dispose();
    assert.equal(first.currentRepo(), undefined);
    assert.equal(second.currentRepo(), secondRepo);
    comparisons.dispose();
    assert.equal(second.currentRepo(), undefined);
  } finally {
    Module._load = load;
    delete require.cache[require.resolve('../src/graph-provider')];
  }
});
