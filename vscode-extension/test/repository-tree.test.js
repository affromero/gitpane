'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const Module = require('node:module');
const { promisify } = require('node:util');

test('removed repositories disappear from Gitpane while remaining open in Source Control', async () => {
  const root = path.resolve('outside-workspace');
  let discovered = [root];
  let failDiscovery = false;
  const event = () => ({ dispose() {} });
  const uri = value => ({ fsPath: value, toString: () => value });
  const repo = {
    rootUri: uri(root),
    state: { onDidChange: event, worktrees: [{ path: root }] },
    async status() {}
  };
  const api = {
    repositories: [],
    onDidOpenRepository: event,
    onDidCloseRepository: event,
    async openRepository(value) {
      if (value.fsPath === root && !this.repositories.length) this.repositories.push(repo);
      return value.fsPath === root ? repo : null;
    }
  };
  const vscode = {
    EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    Uri: { file: uri },
    workspace: { workspaceFolders: [], getConfiguration: () => ({ get: () => 'gitpane' }) }
  };
  const execFile = () => {};
  execFile[promisify.custom] = async () => {
    if (failDiscovery) throw new Error('unavailable');
    return { stdout: JSON.stringify({ version: 1, repositories: discovered }) };
  };
  const load = Module._load;
  let RepositoryTree;
  try {
    Module._load = function (name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === 'node:child_process') return { execFile };
      return load.call(this, name, ...args);
    };
    ({ RepositoryTree } = require('../src/extension'));
  } finally {
    Module._load = load;
  }
  const tree = new RepositoryTree(api, { appendLine() {} });
  try {
    await tree.refresh();
    assert.deepEqual(tree.getChildren().filter(node => node.type === 'repo').map(node => node.repo.rootUri.fsPath), [root]);
    failDiscovery = true;
    await tree.refresh();
    assert.equal(tree.getChildren().some(node => node.type === 'repo'), true);
    assert.match(tree.getChildren().find(node => node.type === 'error').message, /stale/);
    failDiscovery = false;
    discovered = [];
    await tree.refresh();
    assert.equal(tree.getChildren().some(node => node.type === 'repo'), false);
    assert.deepEqual(api.repositories, [repo]);
  } finally {
    tree.dispose();
  }
});
