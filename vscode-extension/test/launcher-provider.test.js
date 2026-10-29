'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { LauncherProvider } = require('../src/launcher-provider');

function fixture() {
  const root = '/workspace/repo $(touch unexpected)';
  const repo = { rootUri: { fsPath: root, toString: () => `file://${root}` } };
  let incarnation = 1;
  const tree = { git: { repositories: [repo] }, isVisible: () => true,
    repositoryToken: () => incarnation, isCurrentRepository: (item, token) => token === incarnation && tree.isVisible(item) };
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
  return { repo, root, tree, action, opened, errors, calls, backend, window, provider, reopen() { incarnation++; } };
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

test('a removed worktree link during launcher validation prevents launch', async () => {
  const f = fixture();
  let linked = true;
  f.backend.validateLauncher = async () => { linked = false; return { valid: true }; };
  await f.provider.open(f.repo, () => linked);
  assert.deepEqual(f.opened, []);
  assert.match(f.errors[0], /workspace changed/i);
});

test('reopening the same repository path during launcher validation prevents launch', async () => {
  const f = fixture();
  f.backend.validateLauncher = async () => { f.reopen(); return { valid: true }; };
  await f.provider.open(f.repo);
  assert.deepEqual(f.opened, []);
  assert.match(f.errors[0], /workspace changed/i);
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

test('configured multiplexer placement uses its verified target without a placement picker', async () => {
  const f = fixture(), identity = 'a'.repeat(64), token = 'b'.repeat(64), execution = [];
  f.provider.vscode.workspace = { getConfiguration: () => ({ get: () => 'herdr' }) };
  f.backend.planLauncher = async () => ({ kind: 'multiplexer', fingerprint: 'original', repo_identity: identity });
  f.backend.launcherChoices = async (...args) => {
    assert.deepEqual(args, [f.root, 'editor', 'original', 'herdr']);
    return { repo_identity: identity, notice: null, choices: [{ id: 'configured', label: 'Configured target', context_token: token }] };
  };
  f.backend.executeLauncher = async (...args) => { execution.push(args.slice(0, -1)); assert.equal(args.at(-1)(), true); return { launched: true }; };
  let pickCount = 0;
  f.window.showQuickPick = async choices => { pickCount++; return choices[0]; };
  await f.provider.open(f.repo);
  assert.equal(pickCount, 1);
  assert.deepEqual(execution, [[f.root, 'editor', 'original', identity, 'herdr', 'configured', token]]);
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.errors, []);
});

test('ask placement passes the selected backend, target and context token', async () => {
  for (const kind of ['tmux', 'herdr']) {
    const f = fixture(), identity = 'a'.repeat(64), first = 'b'.repeat(64), second = 'c'.repeat(64), execution = [];
    f.backend.planLauncher = async () => ({ kind: 'ask', fingerprint: 'original', repo_identity: identity });
    f.backend.launcherChoices = async (_repo, _id, _fingerprint, backend) => {
      assert.equal(backend, kind);
      return { repo_identity: identity, choices: [
        { id: 'left', label: 'Left', context_token: first },
        { id: 'right', label: 'Right', context_token: second }
      ] };
    };
    f.backend.executeLauncher = async (...args) => { execution.push(args.slice(0, -1)); assert.equal(args.at(-1)(), true); return { launched: true }; };
    f.window.showQuickPick = async choices => choices.find(choice => choice.kind === kind) ||
      choices.find(choice => choice.id === 'right') || choices[0];
    await f.provider.open(f.repo);
    assert.deepEqual(execution, [[f.root, 'editor', 'original', identity, kind, 'right', second]]);
    assert.deepEqual(f.opened, []);
    assert.deepEqual(f.errors, []);
  }
});

test('cancelled or forged multiplexer selections do not launch', async () => {
  for (const phase of ['backend', 'placement', 'forged-backend', 'forged-placement']) {
    const f = fixture(), identity = 'a'.repeat(64), token = 'b'.repeat(64);
    f.backend.planLauncher = async () => ({ kind: 'ask', fingerprint: 'original', repo_identity: identity });
    f.backend.launcherChoices = async () => ({ repo_identity: identity, choices: [{ id: 'one', label: 'One', context_token: token }] });
    f.backend.executeLauncher = async () => { throw new Error('unexpected launch'); };
    let pick = 0;
    f.window.showQuickPick = async choices => {
      pick++;
      if ((pick === 2 && phase === 'backend') || (pick === 3 && phase === 'placement')) return undefined;
      if ((pick === 2 && phase === 'forged-backend') || (pick === 3 && phase === 'forged-placement')) return { ...choices[0] };
      return choices[0];
    };
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, [], phase);
    assert.deepEqual(f.errors, [], phase);
  }
});

test('placement notice remains visible while available choices can be selected', async () => {
  const f = fixture(), identity = 'a'.repeat(64), token = 'b'.repeat(64), execution = [];
  f.provider.vscode.workspace = { getConfiguration: () => ({ get: () => 'herdr' }) };
  f.backend.planLauncher = async () => ({ kind: 'ask', fingerprint: 'original', repo_identity: identity });
  f.backend.launcherChoices = async () => ({ repo_identity: identity, notice: 'New tabs are unavailable.',
    choices: [{ id: 'split', label: 'Split', context_token: token }] });
  f.backend.executeLauncher = async (...args) => { execution.push(args.slice(0, -1)); return { launched: true }; };
  let placeholder;
  f.window.showQuickPick = async (choices, options) => {
    if (choices[0].id === 'split') placeholder = options.placeHolder;
    return choices[0];
  };
  await f.provider.open(f.repo);
  assert.match(placeholder, /New tabs are unavailable/);
  assert.equal(execution[0][5], 'split');
  assert.deepEqual(f.errors, []);
});

test('invalid or stale multiplexer choices never launch', async () => {
  const identity = 'a'.repeat(64), token = 'b'.repeat(64);
  for (const result of [
    { repo_identity: 'c'.repeat(64), choices: [{ id: 'configured', label: 'Target', context_token: token }] },
    { repo_identity: identity, choices: [{ id: 'configured', label: 'Target', context_token: 'invalid' }] },
    { repo_identity: identity, choices: [{ id: 'other', label: 'Target', context_token: token }] },
    { repo_identity: identity, choices: [{ id: 'configured', label: 'One', context_token: token },
      { id: 'configured', label: 'Two', context_token: token }] }
  ]) {
    const f = fixture();
    f.backend.planLauncher = async () => ({ kind: 'multiplexer', fingerprint: 'original', repo_identity: identity });
    f.backend.launcherChoices = async () => result;
    f.backend.executeLauncher = async () => { throw new Error('unexpected launch'); };
    await f.provider.open(f.repo);
    assert.deepEqual(f.opened, []);
    assert.match(f.errors[0], /invalid|changed/i);
  }
});

test('repository or worktree changes during multiplexer selection prevent execution', async () => {
  for (const phase of ['backend', 'choices', 'placement', 'validation']) {
    const f = fixture(), identity = 'a'.repeat(64), token = 'b'.repeat(64);
    let linked = true, pick = 0, executed = false;
    f.backend.planLauncher = async () => ({ kind: 'ask', fingerprint: 'original', repo_identity: identity });
    f.backend.launcherChoices = async () => {
      if (phase === 'choices') linked = false;
      return { repo_identity: identity, choices: [{ id: 'one', label: 'One', context_token: token }] };
    };
    f.backend.validateLauncher = async () => { if (phase === 'validation') f.reopen(); return { valid: true }; };
    f.backend.executeLauncher = async () => { executed = true; return { launched: true }; };
    f.window.showQuickPick = async choices => {
      pick++;
      if ((phase === 'backend' && pick === 2) || (phase === 'placement' && pick === 3)) linked = false;
      return choices[0];
    };
    await f.provider.open(f.repo, () => linked);
    assert.equal(executed, false, phase);
    assert.match(f.errors[0], /workspace changed/i, phase);
  }
});

test('multiplexer execution reports partial or uncertain outcomes', async () => {
  for (const code of ['partial_completion', 'outcome_unknown']) {
    const f = fixture(), identity = 'a'.repeat(64), token = 'b'.repeat(64);
    f.provider.vscode.workspace = { getConfiguration: () => ({ get: () => 'tmux' }) };
    f.backend.planLauncher = async () => ({ kind: 'multiplexer', fingerprint: 'original', repo_identity: identity });
    f.backend.launcherChoices = async () => ({ repo_identity: identity, choices: [{ id: 'configured', label: 'Target', context_token: token }] });
    f.backend.executeLauncher = async () => { throw Object.assign(new Error(`launch ${code}`), { code }); };
    await f.provider.open(f.repo);
    assert.match(f.errors[0], new RegExp(code));
    assert.deepEqual(f.opened, []);
  }
});
