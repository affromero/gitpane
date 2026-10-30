'use strict';

const assert = require('node:assert/strict');

module.exports = async function settingsScenario({ vscode, provider, snapshots, preferences,
  configurationChanged, commands, settingUpdates, workspaceState, errors, choose }) {
  const update = async (key, value) => vscode.workspace.getConfiguration('gitpane').update(key, value, 2);
  const nativeExpansion = snapshots.at(-1).expandWorktrees;
  await update('worktrees.defaultVisibility', 'collapsed');
  assert.equal(snapshots.at(-1).expandWorktrees, false);
  await update('worktrees.defaultVisibility', 'expanded');
  assert.equal(snapshots.at(-1).expandWorktrees, true);
  await update('worktrees.defaultVisibility', 'native');
  assert.equal(snapshots.at(-1).expandWorktrees, nativeExpansion);

  await update('sortOrder', 'alphabetical');
  assert.equal(provider.tree.sortOrder, 'alphabetical');
  choose(items => items.find(item => item.order === 'reverse'));
  await commands.get('gitpane.sort')();
  assert.deepEqual(settingUpdates.at(-1), { key: 'sortOrder', value: 'reverse', target: 2 });
  assert.equal(provider.tree.sortOrder, 'reverse');
  await update('sortOrder', 'remember');
  choose(items => items.find(item => item.order === 'dirty'));
  await commands.get('gitpane.sort')();
  assert.equal(preferences.get('sortOrder'), 'remember');
  assert.equal(workspaceState.get('gitpane.sortOrder'), 'dirty');
  assert.equal(provider.tree.sortOrder, 'dirty');

  await update('refreshInterval', 17);
  assert.equal(provider.tree.refreshInterval, 17000);
  assert.equal(provider.refreshInterval, 17000);
  await update('showSessionLiveness', false);
  assert.equal(provider.tree.liveness.visible, false);
  provider.tree.setVisible(false);
  await update('showSessionLiveness', true);
  assert.equal(provider.tree.liveness.visible, false);
  provider.tree.setVisible(true);
  assert.equal(provider.tree.liveness.visible, true);

  await update('refreshInterval', 'invalid');
  assert.match(errors.pop(), /gitpane.refreshInterval/);
  assert.equal(provider.refreshInterval, 17000);
  assert.equal(provider.tree.refreshInterval, 17000);
  preferences.clear();
  await workspaceState.delete('gitpane.sortOrder');
  configurationChanged.fire({ affectsConfiguration: name => name.startsWith('gitpane.') });
  choose(undefined);
  assert.equal(provider.tree.sortOrder, 'workspace');
  assert.equal(provider.refreshInterval, 5000);
  await provider.tree.refresh();
  console.log('PASS VS Code settings update the running Explorer and preserve workspace sort choices');
};
