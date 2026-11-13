'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vscode = require('vscode');
const { activatedProvider, publishResult } = require('./host');

async function waitFor(check, message) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  throw new Error(message);
}

exports.run = async () => {
  const workspace = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const config = JSON.parse(fs.readFileSync(path.join(workspace, '.gitpane-host-test.json'), 'utf8'));
  const result = config.result;
  try {
    assert.equal(vscode.env.remoteName ?? null, config.remote);
    if (config.backend) {
      await vscode.workspace.getConfiguration('gitpane').update('backendPath', config.backend, vscode.ConfigurationTarget.Global);
    }
    const provider = await activatedProvider(vscode, config.extension);
    await vscode.commands.executeCommand('gitpane.repositories.focus');
    await waitFor(() => provider.view?.visible && provider.ready, 'Gitpane view did not become ready');
    const phase = config.phase;
    if (!fs.existsSync(phase)) {
      const row = await waitFor(() => provider.snapshot().repositories.find(item => item.name === path.basename(config.repo)),
        'Fixture repository was not discovered');
      await provider.loadChanges(row.id);
      await waitFor(() => provider.snapshot().repositories.find(item => item.id === row.id)?.changesPage.state === 'ready',
        'Changes did not load');
      const nodes = [...provider.changes.values()];
      const expected = [];
      for (const [group, name] of [['index', 'staged'], ['working', 'working']]) {
        const node = nodes.find(item => item.group === group && path.basename(item.change.uri.fsPath) === `${name}.txt`);
        assert.ok(node, `Missing ${group} diff`);
        await vscode.commands.executeCommand('gitpane.openChange', node);
        await vscode.commands.executeCommand('workbench.action.keepEditor');
        const input = vscode.window.tabGroups.activeTabGroup.activeTab?.input;
        assert.ok(input instanceof vscode.TabInputTextDiff);
        expected.push({ original: input.original.toString(), modified: input.modified.toString(),
          left: `original ${name}\n`, right: `${name} change\n` });
      }
      fs.writeFileSync(phase, JSON.stringify(expected));
      void vscode.commands.executeCommand('workbench.action.reloadWindow').catch(error => {
        if (error.name !== 'Canceled') publishResult(result, [{ name: 'reload window', ok: false, error: error.stack || String(error) }]);
      });
      return;
    }
    const expected = JSON.parse(fs.readFileSync(phase, 'utf8'));
    await waitFor(() => vscode.window.tabGroups.all.flatMap(group => group.tabs).filter(tab => tab.input instanceof vscode.TabInputTextDiff).length >= expected.length,
      'Diff tabs were not restored after window reload');
    const inputs = vscode.window.tabGroups.all.flatMap(group => group.tabs).map(tab => tab.input)
      .filter(input => input instanceof vscode.TabInputTextDiff);
    for (const entry of expected) {
      const input = inputs.find(item => item.original.toString() === entry.original && item.modified.toString() === entry.modified);
      assert.ok(input, `Missing restored diff ${entry.original}`);
      assert.equal((await vscode.workspace.openTextDocument(input.original)).getText(), entry.left);
      assert.equal((await vscode.workspace.openTextDocument(input.modified)).getText(), entry.right);
    }
    publishResult(result, [{ name: 'restore staged and working diffs after window reload', ok: true }]);
  } catch (error) {
    publishResult(result, [{ name: 'restore staged and working diffs after window reload', ok: false, error: error.stack || String(error) }]);
    throw error;
  }
};
