'use strict';

const vscode = require('vscode');
const assert = require('node:assert/strict');
const { activatedProvider, publishResult } = require('./host');

exports.run = async () => {
  const before = process.memoryUsage().rss;
  const started = process.hrtime.bigint();
  try {
    if (process.env.GITPANE_TEST_BINARY) {
      await vscode.workspace.getConfiguration('gitpane').update('backendPath', process.env.GITPANE_TEST_BINARY, vscode.ConfigurationTarget.Global);
    }
    const provider = await activatedProvider(vscode, process.env.GITPANE_TEST_EXTENSION);
    const tree = provider.tree;
    await vscode.commands.executeCommand('gitpane.repositories.focus');
    const target = Number(process.env.GITPANE_SCALE_COUNT);
    const deadline = Date.now() + 120000;
    let repositories = [];
    while (Date.now() < deadline) {
      repositories = provider.snapshot().repositories;
      if (provider.view?.visible && repositories.length === target && repositories.every(repo =>
        !repo.statusUnavailable && repo.dirty && repo.changeCountLowerBound >= 1)) break;
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    assert.ok(provider.view?.visible, 'Gitpane view must be visible');
    assert.equal(repositories.length, target, 'all workspace repositories must be discovered');
    assert.ok(repositories.every(repo => !repo.statusUnavailable && repo.dirty && repo.changeCountLowerBound >= 1),
      'each fixture repository must report its untracked change');
    const readyMs = Number(process.hrtime.bigint() - started) / 1e6;
    const sample = repositories.slice(0, Math.min(75, repositories.length));
    const expandedStart = process.hrtime.bigint();
    for (const repo of sample) await provider.loadChanges(repo.id);
    const expanded = provider.snapshot().repositories;
    for (const repo of sample) {
      const actual = expanded.find(item => item.id === repo.id);
      assert.equal(actual?.changesPage.state, 'ready', `${repo.name} changes must remain loaded`);
      assert.ok(actual?.groups.some(group => group.files.some(file => file.path === 'new.txt')),
        `${repo.name} must show its untracked file`);
    }
    const expandedMs = Number(process.hrtime.bigint() - expandedStart) / 1e6;
    const refreshStart = process.hrtime.bigint();
    await tree.refresh();
    const refreshMs = Number(process.hrtime.bigint() - refreshStart) / 1e6;
    const nodes = tree.getChildren();
    const errors = nodes.filter(node => node.type === 'error').map(node => node.message);
    assert.deepEqual(errors, [], 'repository discovery and status should succeed');
    publishResult(process.env.GITPANE_TEST_RESULT, {
      repositories: repositories.length,
      readyMs,
      expandedRepositories: sample.length,
      expandedMs,
      refreshMs,
      extensionHostRssBeforeBytes: before,
      extensionHostRssAfterBytes: process.memoryUsage().rss
    });
  } catch (error) {
    publishResult(process.env.GITPANE_TEST_RESULT, { error: error.stack || String(error) });
    throw error;
  }
};
