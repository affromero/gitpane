'use strict';

const vscode = require('vscode');
const path = require('node:path');
const assert = require('node:assert/strict');
const { publishResult } = require('./host');

exports.run = async () => {
  const before = process.memoryUsage().rss;
  const started = process.hrtime.bigint();
  let tree, backend, provider;
  try {
    if (process.env.GITPANE_TEST_BINARY) {
      await vscode.workspace.getConfiguration('gitpane').update('backendPath', process.env.GITPANE_TEST_BINARY, vscode.ConfigurationTarget.Global);
    }
    const git = (await vscode.extensions.getExtension('vscode.git').activate()).getAPI(1);
    const extension = vscode.extensions.getExtension('gitpane.gitpane-explorer');
    assert.ok(extension, 'Gitpane extension must be registered');
    await extension.activate();
    const { RepositoryTree, GitpaneViewProvider } = require(path.join(process.env.GITPANE_TEST_EXTENSION, 'src/extension.js'));
    const { BackendManager } = require(path.join(process.env.GITPANE_TEST_EXTENSION, 'src/backend-manager.js'));
    const output = vscode.window.createOutputChannel('Gitpane scale test');
    backend = new BackendManager({ extensionPath: process.env.GITPANE_TEST_EXTENSION }, { output });
    await backend.ready();
    tree = new RepositoryTree(git, output, backend);
    provider = new GitpaneViewProvider(tree, vscode.Uri.file(process.env.GITPANE_TEST_EXTENSION));
    tree.setVisible(true);
    const target = Number(process.env.GITPANE_SCALE_COUNT);
    const deadline = Date.now() + 120000;
    let repositories = [];
    while (Date.now() < deadline) {
      repositories = provider.snapshot().repositories;
      if (repositories.length === target && repositories.every(repo =>
        !repo.statusUnavailable && repo.dirty && repo.groups.some(group => group.files.some(file => file.path === 'new.txt')))) break;
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    assert.equal(repositories.length, target, 'all workspace repositories must be discovered');
    assert.ok(repositories.every(repo => !repo.statusUnavailable && repo.dirty &&
      repo.groups.some(group => group.files.some(file => file.path === 'new.txt'))),
    'each fixture repository must show its untracked file');
    const readyMs = Number(process.hrtime.bigint() - started) / 1e6;
    const refreshStart = process.hrtime.bigint();
    await tree.refresh();
    const refreshMs = Number(process.hrtime.bigint() - refreshStart) / 1e6;
    const nodes = tree.getChildren();
    const errors = nodes.filter(node => node.type === 'error').map(node => node.message);
    assert.deepEqual(errors, [], 'repository discovery and status should succeed');
    publishResult(process.env.GITPANE_TEST_RESULT, {
      repositories: repositories.length,
      readyMs,
      refreshMs,
      extensionHostRssBeforeBytes: before,
      extensionHostRssAfterBytes: process.memoryUsage().rss
    });
  } catch (error) {
    publishResult(process.env.GITPANE_TEST_RESULT, { error: error.stack || String(error) });
    throw error;
  } finally {
    if (provider) provider.dispose();
    if (tree) tree.dispose();
    if (backend) backend.dispose();
  }
};
