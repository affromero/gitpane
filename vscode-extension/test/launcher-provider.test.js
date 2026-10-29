'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { LauncherProvider } = require('../src/launcher-provider');

function fixture() {
  const root = '/workspace/repo $(touch unexpected)';
  const repo = { rootUri: { fsPath: root, toString: () => `file://${root}` } };
  const tree = { git: { repositories: [repo] }, isVisible: () => true };
  const action = { id: 'editor', label: 'Open editor', kind: 'open', fingerprint: 'original' };
  const opened = [];
  const errors = [];
  const calls = [];
  const backend = {
    async listLaunchers(repoPath) { calls.push(['list', repoPath]); return { actions: [action] }; },
    async planLauncher(repoPath, id, fingerprint) {
      calls.push(['plan', repoPath, id, fingerprint]);
      return { kind: 'argv', argv: ['code', '--folder-uri', root, ''], fingerprint };
    },
    async validateLauncher(repoPath, id, fingerprint) {
      calls.push(['validate', repoPath, id, fingerprint]);
      return { valid: true };
    },
  };
  const window = {
    async showQuickPick(choices) { return choices[0]; },
    async showErrorMessage(message) { errors.push(message); },
    async showInformationMessage() {},
    createTerminal(options) {
      const result = { options, shown: false, disposed: false,
        show() { result.shown = true; }, dispose() { result.disposed = true; } };
      opened.push(result);
      return result;
    },
  };
  const provider = new LauncherProvider(tree, backend, { vscode: { window } });
  return { repo, root, tree, action, opened, errors, calls, backend, window, provider };
}

test('a configured argv launcher starts only after a selection and validation', async () => {
  const f = fixture();
  assert.deepEqual(f.calls, []);
  assert.deepEqual(f.opened, []);
  await f.provider.open(f.repo);
  assert.deepEqual(f.calls, [
    ['list', f.root], ['plan', f.root, 'editor', 'original'], ['validate', f.root, 'editor', 'original'],
  ]);
  assert.deepEqual(f.opened[0].options, {
    name: 'Gitpane: Open editor', cwd: f.repo.rootUri, shellPath: 'code', shellArgs: ['--folder-uri', f.root, ''],
  });
  assert.equal(f.opened[0].shown, true);
  assert.deepEqual(f.errors, []);
});

test('cancelling or returning a forged selection never launches a terminal', async () => {
  for (const choose of [() => undefined, choices => ({ ...choices[0] })]) {
    const f = fixture();
    f.window.showQuickPick = async choices => choose(choices);
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, []);
    assert.deepEqual(f.calls, [['list', f.root]]);
  }
});

test('a configured terminal opens with the repository as its working directory', async () => {
  const f = fixture();
  f.backend.planLauncher = async () => ({ kind: 'terminal', fingerprint: 'original' });
  await f.provider.open(f.repo);
  assert.deepEqual(f.opened[0].options, { name: 'Gitpane: Open editor', cwd: f.repo.rootUri });
});

test('shell launch plans keep paths in the environment instead of shell arguments', { skip: process.platform === 'win32' }, async () => {
  const f = fixture();
  const env = { GITPANE_LAUNCH_PATH: f.root, GITPANE_LAUNCH_BASE: '' };
  f.backend.planLauncher = async () => ({ kind: 'shell', fingerprint: 'original', script: 'editor "$GITPANE_LAUNCH_PATH"', env });
  await f.provider.open(f.repo);
  assert.deepEqual(f.opened[0].options, {
    name: 'Gitpane: Open editor', cwd: f.repo.rootUri, shellPath: 'sh',
    shellArgs: ['-c', 'editor "$GITPANE_LAUNCH_PATH"'], env,
  });
});

test('invalid launch plans and shell interpreters fail without creating terminals', async () => {
  const plans = [
    { kind: 'argv', argv: ['bash', '-c', 'echo unsafe'] },
    { kind: 'argv', argv: ['C:\\Windows\\System32\\cmd.exe', '/c', 'echo unsafe'] },
    { kind: 'argv', argv: ['python3.14', '-c', 'pass'] },
    { kind: 'argv', argv: ['env', 'sh'] },
    { kind: 'argv', argv: ['code', 'bad\0argument'] },
    { kind: 'argv', argv: [] },
    { kind: 'external' },
    { kind: 'shell', script: 'true', env: { GITPANE_LAUNCH_PATH: '/repo', GITPANE_LAUNCH_BASE: '', OTHER: 'value' } },
    { kind: 'shell', script: 'true', env: { GITPANE_LAUNCH_PATH: '/repo', GITPANE_LAUNCH_BASE: 'unexpected' } },
  ];
  for (const plan of plans) {
    const f = fixture();
    f.backend.planLauncher = async () => ({ ...plan, fingerprint: 'original' });
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, [], JSON.stringify(plan));
    assert.match(f.errors[0], /unsupported|invalid/i);
  }
});

test('changed configuration and malformed lists cannot launch an action', async () => {
  for (const change of [
    f => { f.backend.planLauncher = async () => ({ kind: 'terminal', fingerprint: 'changed' }); },
    f => { f.backend.validateLauncher = async () => ({ valid: false }); },
    f => { f.backend.listLaunchers = async () => ({ actions: [f.action, f.action] }); },
  ]) {
    const f = fixture();
    change(f);
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, []);
    assert.match(f.errors[0], /changed|invalid/i);
  }
});

test('scope changes and disposal during selection or planning prevent launches', async () => {
  for (const phase of ['selection', 'planning', 'validation', 'disposal']) {
    const f = fixture();
    if (phase === 'selection') f.window.showQuickPick = async choices => { f.tree.git.repositories = []; return choices[0]; };
    if (phase === 'planning') f.backend.planLauncher = async () => { f.tree.isVisible = () => false; return { kind: 'terminal', fingerprint: 'original' }; };
    if (phase === 'validation') f.backend.validateLauncher = async () => { f.tree.git.repositories = []; return { valid: true }; };
    if (phase === 'disposal') f.backend.planLauncher = async () => { f.provider.dispose(); return { kind: 'terminal', fingerprint: 'original' }; };
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, [], phase);
    assert.match(f.errors[0], /workspace changed/i);
  }
});

test('failed terminal presentation disposes the created terminal and reports the error', async () => {
  const f = fixture();
  f.window.createTerminal = options => {
    const terminal = { options, disposed: false, show() { throw new Error('unavailable'); }, dispose() { this.disposed = true; } };
    f.opened.push(terminal);
    return terminal;
  };
  await f.provider.open(f.repo);
  assert.equal(f.opened[0].disposed, true);
  assert.match(f.errors[0], /unavailable/);
});
