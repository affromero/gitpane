'use strict';

const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { activatedProvider, publishResult } = require('./host');

exports.run = async () => {
  const results = [];
  const fixture = process.env.GITPANE_TEST_REPO;
  async function test(name, action) {
    try { await action(); results.push({ name, ok: true }); }
    catch (error) { results.push({ name, ok: false, error: error.stack || String(error) }); }
  }
  async function waitFor(condition, message, timeout = 20000) {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (condition()) return;
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    const detail = typeof message === 'function' ? message() : message;
    assert.fail(`${detail}; window focused: ${vscode.window.state.focused}; git.autorefresh: ${vscode.workspace.getConfiguration('git').get('autorefresh')}`);
  }
  function samePath(first, second) {
    const left = fs.realpathSync(first);
    const right = fs.realpathSync(second);
    return process.platform === 'win32' ? left.toLowerCase() === right.toLowerCase() : left === right;
  }
  let api, tree, provider, nodes;
  const rootNode = target => tree.getChildren().find(item => item.type === 'repo' && samePath(item.repo.rootUri.fsPath, target));
  const snapshotRepo = target => provider.snapshot().repositories.find(item => item.name === path.basename(target));

  await test('show repositories in nested and multi-root workspaces', async () => {
    if (process.env.GITPANE_TEST_BINARY) {
      await vscode.workspace.getConfiguration('gitpane').update('backendPath', process.env.GITPANE_TEST_BINARY, vscode.ConfigurationTarget.Global);
    }
    api = (await vscode.extensions.getExtension('vscode.git').activate()).getAPI(1);
    provider = await activatedProvider(vscode, process.env.GITPANE_TEST_EXTENSION);
    tree = provider.tree;
    await vscode.commands.executeCommand('gitpane.repositories.focus');
    await waitFor(() => provider.view?.visible, 'Gitpane view was not visible');
    await waitFor(() => [fixture, process.env.GITPANE_TEST_NESTED, process.env.GITPANE_TEST_SECOND]
      .every(target => rootNode(target)), () => `workspace repositories were not discovered; expected: ${JSON.stringify([fixture, process.env.GITPANE_TEST_NESTED, process.env.GITPANE_TEST_SECOND])}; VS Code repositories: ${JSON.stringify(api.repositories.map(item => item.rootUri.fsPath))}; tree: ${JSON.stringify(tree.getChildren().filter(item => item.type === 'repo').map(item => item.repo.rootUri.fsPath))}; workspace: ${JSON.stringify(vscode.workspace.workspaceFolders.map(folder => folder.uri.fsPath))}; repositoryScanMaxDepth: ${vscode.workspace.getConfiguration('git').get('repositoryScanMaxDepth')}`, 30000);
    await waitFor(() => snapshotRepo(fixture)?.dirty, 'repository changes were not loaded');
    await provider.loadChanges(snapshotRepo(fixture).id);
    await waitFor(() => snapshotRepo(fixture)?.changesPage.state === 'ready', 'repository changes page was not loaded');
    provider.snapshot();
    nodes = [...provider.changes.values()].filter(node => samePath(node.repo.rootUri.fsPath, fixture));
  });

  await test('ignore repositories outside the workspace', async () => {
    assert.ok(api && tree, 'repository tree was not available');
    const outside = await api.openRepository(vscode.Uri.file(process.env.GITPANE_TEST_OUTSIDE));
    assert.ok(outside, 'control repository must be open in VS Code');
    await waitFor(() => api.repositories.some(repo => samePath(repo.rootUri.fsPath, process.env.GITPANE_TEST_OUTSIDE)), 'control repository did not open');
    assert.ok(!rootNode(process.env.GITPANE_TEST_OUTSIDE), 'outside repository appeared in Gitpane');
  });

  const cases = [
    ['staged', 'index', 'staged.txt', 'original staged\n', 'staged value\n'],
    ['unstaged', 'working', 'working.txt', 'original working\n', 'working value\n'],
    ['partially staged index', 'index', 'partial.txt', 'original partial\n', 'index partial\n'],
    ['partially staged working file', 'working', 'partial.txt', 'index partial\n', 'working partial\n'],
    ['rename', 'index', 'renamed.txt', 'original original\n', 'original original\n'],
    ['delete', 'working', 'deleted.txt', 'original deleted\n', ''],
    ['untracked', 'untracked', 'untracked.txt', '', 'untracked value\n']
  ];
  for (const [name, group, filename, left, right] of cases) {
    await test(`open ${name} diff`, async () => {
      assert.ok(nodes, 'repository tree was not available');
      const node = nodes.find(item => item.group === group && path.basename(item.change.uri.fsPath) === filename);
      assert.ok(node, `missing ${group} ${filename}`);
      assert.ok(snapshotRepo(fixture).groups.some(item => item.id === group && item.files.some(file => file.name === filename)));
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      assert.equal(vscode.window.tabGroups.activeTabGroup.activeTab, undefined, 'previous editor must be closed');
      await vscode.commands.executeCommand('gitpane.openChange', node);
      const tab = vscode.window.tabGroups.activeTabGroup.activeTab;
      assert.ok(tab && tab.input instanceof vscode.TabInputTextDiff, `expected text diff for ${name}`);
      assert.equal((await vscode.workspace.openTextDocument(tab.input.original)).getText(), left);
      assert.equal((await vscode.workspace.openTextDocument(tab.input.modified)).getText(), right);
    });
  }

  await test('workspace repository updates after a file is created', async () => {
    assert.ok(tree);
    fs.writeFileSync(path.join(fixture, 'later.txt'), 'later\n');
    await waitFor(() => {
      const repo = snapshotRepo(fixture);
      return repo && repo.groups.some(group => group.files.some(file => file.name === 'later.txt'));
    }, 'new file was not visible');
  });

  await test('adding and removing a workspace folder updates the tree automatically', async () => {
    assert.ok(tree);
    const added = vscode.Uri.file(process.env.GITPANE_TEST_ADDED);
    const index = vscode.workspace.workspaceFolders.length;
    assert.equal(vscode.workspace.updateWorkspaceFolders(index, 0, { uri: added }), true);
    try {
      await waitFor(() => rootNode(process.env.GITPANE_TEST_ADDED), 'added workspace repository did not appear', 30000);
    } finally {
      const position = (vscode.workspace.workspaceFolders || []).findIndex(folder => samePath(folder.uri.fsPath, added.fsPath));
      if (position >= 0) assert.equal(vscode.workspace.updateWorkspaceFolders(position, 1), true);
    }
    await waitFor(() => !rootNode(process.env.GITPANE_TEST_ADDED), 'removed workspace repository remained visible');
  });

  publishResult(process.env.GITPANE_TEST_RESULT, results);
  if (results.some(result => !result.ok)) throw new Error('Gitpane integration tests failed.');
};
