'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ViewSettings, readPreferences } = require('../src/settings');

const configuration = values => ({ get: (key, fallback) => Object.hasOwn(values, key) ? values[key] : fallback });

test('unset preferences retain native values and remembered workspace sorting', () => {
  assert.deepEqual(readPreferences(configuration({}), 'dirty'), {
    sortOrder: 'dirty', refreshInterval: 5000, worktreeExpansion: undefined,
    showSessionLiveness: true, graphFirstParent: false, graphLabelMaxLength: undefined
  });
  assert.equal(readPreferences(configuration({}), 'obsolete').sortOrder, 'workspace');
  assert.deepEqual(readPreferences(configuration({ sortOrder: 'reverse', refreshInterval: 1,
    'worktrees.defaultVisibility': 'collapsed', showSessionLiveness: false,
    'graph.firstParent': true, 'graph.labelMaxLength': 0 }), 'dirty'), {
    sortOrder: 'reverse', refreshInterval: 1000, worktreeExpansion: false,
    showSessionLiveness: false, graphFirstParent: true, graphLabelMaxLength: 0
  });
});

test('invalid preferences fail visibly rather than substituting defaults', () => {
  for (const [key, values] of Object.entries({ sortOrder: ['invalid', null], refreshInterval: [0, 301, 1.5, '5'],
    'worktrees.defaultVisibility': [true, 'invalid'], showSessionLiveness: [0, 'true'],
    'graph.firstParent': [null, 1], 'graph.labelMaxLength': [-2, 121, 1.5] })) {
    for (const value of values) assert.throws(() => readPreferences(configuration({ [key]: value })),
      error => error.message.includes(`gitpane.${key}`));
  }
});

test('settings are discoverable in the manifest at their supported scopes', () => {
  const { contributes } = require('../package.json');
  const properties = contributes.configuration.properties;
  for (const key of ['sortOrder', 'worktrees.defaultVisibility', 'refreshInterval', 'showSessionLiveness',
    'graph.firstParent', 'graph.labelMaxLength', 'sessionBackend']) {
    assert.equal(properties[`gitpane.${key}`].scope, 'window');
  }
  assert.equal(properties['gitpane.backendPath'].scope, 'machine');
  assert.equal(properties['gitpane.sessionBackend'].ignoreSync, true);
  assert.ok(contributes.commands.some(item => item.command === 'gitpane.openSettings'));
  assert.ok(contributes.menus['view/title'].some(item => item.command === 'gitpane.openSettings' &&
    item.when === 'view == gitpane.repositories'));
});

test('open settings searches the running extension identity after a publisher migration', async () => {
  let openSettings;
  const requests = [];
  const disposable = { dispose() {} };
  const vscode = {
    workspace: { onDidChangeConfiguration: () => disposable,
      getConfiguration: () => configuration({}) },
    commands: {
      registerCommand(name, handler) {
        assert.equal(name, 'gitpane.openSettings');
        openSettings = handler;
        return disposable;
      },
      executeCommand: async (...args) => requests.push(args)
    }
  };
  const context = { extension: { id: 'affromero.gitpane-vscode' } };
  const tree = { configureView() {}, setSortOrder() {} };
  const view = { configureView() {} };
  const settings = new ViewSettings(vscode, context, tree, view, view);
  try {
    await openSettings();
    assert.deepEqual(requests, [['workbench.action.openSettings', '@ext:affromero.gitpane-vscode']]);
  } finally { settings.dispose(); }
});
