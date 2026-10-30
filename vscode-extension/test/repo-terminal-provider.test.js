'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { RepoTerminalProvider } = require('../src/repo-terminal-provider');

function fixture() {
  const root = path.resolve('workspace/repo$(fake)\nname');
  const uri = { fsPath: root, toString: () => root };
  const repo = { rootUri: uri };
  const calls = [];
  const errors = [];
  let visible = true;
  let incarnation = 1;
  let failShow = false;
  const tree = { git: { repositories: [repo] }, isVisible: () => visible,
    repositoryToken: () => incarnation, isCurrentRepository: (_, token) => visible && token === incarnation };
  const vscode = { window: {
    createTerminal(options) {
      calls.push(['create', options]);
      return { show() { calls.push(['show']); if (failShow) throw new Error('show failed'); }, dispose() { calls.push(['dispose']); } };
    },
    async showErrorMessage(message) { errors.push(message); }
  } };
  return { provider: new RepoTerminalProvider(tree, { vscode }), repo, uri, tree, calls, errors,
    setVisible(value) { visible = value; }, setFailShow(value) { failShow = value; } };
}

test('opens a new integrated terminal in the selected repository without running a command', async () => {
  const f = fixture();
  await f.provider.open(f.repo);
  assert.deepEqual(f.calls, [
    ['create', { name: 'Gitpane: repo＄(fake) name', cwd: f.uri }], ['show']
  ]);
  assert.deepEqual(f.errors, []);
});

test('does not create a terminal after the repository is removed, hidden or disposed', async () => {
  const f = fixture();
  f.setVisible(false);
  await f.provider.open(f.repo);
  f.setVisible(true);
  f.tree.git.repositories = [];
  await f.provider.open(f.repo);
  f.tree.git.repositories = [f.repo];
  f.provider.dispose();
  await f.provider.open(f.repo);
  assert.deepEqual(f.calls, []);
  assert.equal(f.errors.length, 3);
  assert.ok(f.errors.every(message => /no longer in the workspace/i.test(message)));
});

test('does not create a terminal for a removed worktree link', async () => {
  const f = fixture();
  await f.provider.open(f.repo, () => false);
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /no longer in the workspace/i);
});

test('reports terminal creation errors and disposes an unusable terminal', async () => {
  const f = fixture();
  f.setFailShow(true);
  await f.provider.open(f.repo);
  assert.equal(f.calls.at(-1)[0], 'dispose');
  assert.match(f.errors[0], /show failed/);
});
