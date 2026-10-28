'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const Module = require('node:module');

test('workspace repositories appear automatically and reconcile while visible', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const workspace = path.resolve('workspace');
  const otherWorkspace = path.resolve('other-workspace');
  const root = path.join(workspace, 'repo');
  const addedRoot = path.join(workspace, 'nested', 'repo');
  const otherRoot = path.join(otherWorkspace, 'other');
  let failed = false;
  let addedFile = false;
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
  const tree = new RepositoryTree(api, { appendLine() {} });
  const provider = new GitpaneViewProvider(tree, '/extension');
  const countFiles = repo => new Set(repo.groups.flatMap(group => group.files.map(file => file.path))).size;
  const settle = () => new Promise(resolve => setImmediate(resolve));
  try {
    await tree.refresh();
    assert.deepEqual(tree.getChildren().filter(node => node.type === 'repo').map(node => node.repo.rootUri.fsPath), [root]);
    assert.equal(countFiles(provider.snapshot().repositories[0]), 1);

    api.repositories.push(added);
    onOpen(added);
    t.mock.timers.tick(100);
    await settle();
    const rows = provider.snapshot().repositories;
    assert.equal(rows.length, 2);
    assert.equal(new Set(rows.map(node => node.name)).size, 2);
    assert.equal(rows.find(node => node.branch === 'feature').worktree, true);

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
