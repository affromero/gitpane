'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { SubmodulePins, readPins } = require('../src/submodule-pins');

async function waitFor(predicate) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('Pinned submodule did not settle');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

function fixture(t, initiallyOpen = false) {
  const temporary = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-pin-')));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const root = path.join(temporary, 'root');
  const child = path.join(root, 'child');
  const nested = path.join(child, 'nested');
  fs.mkdirSync(nested, { recursive: true });
  const uri = fsPath => ({ fsPath, scheme: 'file', toString: () => `file:${fsPath}` });
  const rootRepo = { rootUri: uri(root), state: {} };
  const childRepo = { rootUri: uri(child), state: {} };
  const nestedRepo = { rootUri: uri(nested), state: {} };
  const available = new Map([[root, rootRepo], [child, childRepo], [nested, nestedRepo]]);
  const tokens = new Map([[root, 1], ...(initiallyOpen ? [[child, 1]] : [])]);
  const revisions = new Map(tokens);
  const listeners = new Set();
  let visible = true;
  let saved;
  let failSave;
  let onSave = async () => {};
  let opens = 0;
  const state = { get: () => saved, async update(_key, value) {
    if (failSave) throw failSave;
    saved = structuredClone(value);
    await onSave(value);
  } };
  const tree = { onDidChangeTreeData(handler) { listeners.add(handler); return { dispose: () => listeners.delete(handler) }; },
    changed() { for (const handler of listeners) handler(); },
    repositoryToken: repo => tokens.get(repo?.rootUri.fsPath),
    repositoryRevision: uriValue => revisions.get(uriValue.fsPath) || 0,
    isCurrentRepository: (repo, tokenValue) => visible && !!repo && tokenValue !== undefined &&
      tokens.get(repo.rootUri.fsPath) === tokenValue && git.repositories.includes(repo),
    isPathInWorkspace: value => visible && (value === root || value.startsWith(root + path.sep)) };
  const git = { repositories: initiallyOpen ? [rootRepo, childRepo] : [rootRepo], async openRepository(value) {
    opens++;
    const repo = available.get(value.fsPath);
    if (!repo) return undefined;
    if (!this.repositories.includes(repo)) {
      this.repositories.push(repo);
      tokens.set(value.fsPath, (tokens.get(value.fsPath) || 0) + 1);
      revisions.set(value.fsPath, (revisions.get(value.fsPath) || 0) + 1);
    }
    return repo;
  } };
  const ids = { root: '1'.repeat(64), child: '2'.repeat(64), nested: '3'.repeat(64) };
  const first = { path: 'child', parent_identity: ids.root, child_identity: ids.child };
  const second = { path: 'nested', parent_identity: ids.child, child_identity: ids.nested };
  let validation = async (_root, chain) => {
    const paths = [root];
    for (const link of chain) paths.push(path.join(paths.at(-1), link.path));
    return { repos: paths };
  };
  let sessions = 0, sessionCloses = 0;
  const backend = { generation: 1, validateSubmodulePin: (...args) => validation(...args),
    async openPinValidationSession() {
      sessions++;
      return { validate: (...args) => validation(...args), async close() { sessionCloses++; } };
    } };
  let pick = choices => choices[0];
  let confirm = () => undefined;
  const manager = options => {
    const pins = new SubmodulePins(tree, backend, git, state, { Uri: { file: uri }, window: {
      showQuickPick: async choices => pick(choices), async showInformationMessage() {},
      showWarningMessage: async (...args) => confirm(...args)
    } }, options);
    pins.setVisible(true);
    return pins;
  };
  return { root, child, nested, first, second, rootRepo, childRepo, nestedRepo, tree, git, state, backend, manager,
    get saved() { return saved; }, get opens() { return opens; }, get sessions() { return sessions; }, get sessionCloses() { return sessionCloses; },
    setVisible(value) { visible = value; tree.changed(); }, setValidation(value) { validation = value; },
    setSaved(value) { saved = value; }, setFailure(value) { failSave = value; },
    setPick(value) { pick = value; },
    setConfirm(value) { confirm = value; },
    onSave(value) { onSave = value; },
    resetOpen() { git.repositories = [rootRepo]; tokens.delete(child); tokens.delete(nested); revisions.delete(child); revisions.delete(nested); }
  };
}

test('pins persist durable nested identities and restore after normal repository changes', async t => {
  const f = fixture(t);
  const firstManager = f.manager();
  t.after(() => firstManager.dispose());
  await firstManager.pin(f.root, [f.first, f.second], () => true);
  assert.equal(firstManager.isPinned(f.nestedRepo), true);
  assert.deepEqual(f.saved, { version: 1, pins: [{ root: f.root, chain: [f.first, f.second] }] });
  firstManager.dispose();
  f.resetOpen();
  const restored = f.manager();
  t.after(() => restored.dispose());
  await waitFor(() => restored.isPinned(f.nestedRepo));
  assert.equal(f.opens, 2, 'only the explicitly pinned repository is reopened');
  assert.deepEqual(f.git.repositories.map(repo => repo.rootUri.fsPath), [f.root, f.nested]);
});

test('unpin removes the marker without hiding or closing discovered repositories', async t => {
  for (const alreadyOpen of [false, true]) {
    const f = fixture(t, alreadyOpen);
    const pins = f.manager();
    t.after(() => pins.dispose());
    await pins.pin(f.root, [f.first], () => true);
    assert.equal(pins.isPinned(f.childRepo), true);
    await pins.unpin(f.childRepo);
    assert.ok(f.git.repositories.includes(f.childRepo), 'unpin never closes the VS Code repository');
    assert.equal(pins.isPinned(f.childRepo), false);
    assert.deepEqual(f.saved.pins, []);
  }
});

test('invalid saved state and storage errors are explicit and never create pins', async t => {
  assert.throws(() => readPins({ version: 9, pins: [] }), /invalid/i);
  assert.throws(() => readPins({ version: 1, pins: [{ root: '/tmp', chain: [{ path: '../escape', parent_identity: '1'.repeat(64), child_identity: '2'.repeat(64) }] }] }), /invalid/i);
  const f = fixture(t);
  const pins = f.manager();
  t.after(() => pins.dispose());
  f.setFailure(new Error('workspace storage failed'));
  await assert.rejects(pins.pin(f.root, [f.first], () => true), /workspace storage failed/);
  assert.equal(pins.isPinned(f.childRepo), false);
  assert.equal(f.saved, undefined);
});

test('workspace changes during validation or storage never publish a stale pin', async t => {
  const f = fixture(t);
  const pins = f.manager();
  t.after(() => pins.dispose());
  let release;
  f.setValidation(() => new Promise(resolve => { release = resolve; }));
  const pending = pins.pin(f.root, [f.first], () => true);
  await waitFor(() => !!release);
  f.setVisible(false);
  release({ repos: [f.root, f.child] });
  await assert.rejects(pending, /workspace|root/i);
  assert.equal(f.opens, 0);
  assert.equal(f.saved, undefined);
});

test('concurrent duplicate pin requests share the saved pin and open only once', async t => {
  const f = fixture(t);
  const pins = f.manager();
  t.after(() => pins.dispose());
  await Promise.all([pins.pin(f.root, [f.first], () => true), pins.pin(f.root, [f.first], () => true)]);
  assert.equal(f.opens, 1);
  assert.equal(f.saved.pins.length, 1);
});

test('a symlink escaping the workspace cannot be pinned even with a malformed backend response', { skip: process.platform === 'win32' }, async t => {
  const f = fixture(t);
  const outside = path.join(path.dirname(f.root), 'outside');
  fs.mkdirSync(outside);
  fs.renameSync(f.child, `${f.child}-saved`);
  fs.symlinkSync(outside, f.child);
  f.setValidation(async () => ({ repos: [f.root, outside] }));
  const pins = f.manager();
  t.after(() => pins.dispose());
  await assert.rejects(pins.pin(f.root, [f.first], () => true), /outside the workspace/i);
  assert.equal(f.opens, 0);
  assert.equal(f.saved, undefined);
});

test('a stale selection during storage rolls back the saved pin', async t => {
  const f = fixture(t);
  const pins = f.manager();
  t.after(() => pins.dispose());
  let selected = true;
  f.onSave(async value => { if (value.pins.length) selected = false; });
  await assert.rejects(pins.pin(f.root, [f.first], () => selected), /changed/i);
  assert.deepEqual(f.saved, { version: 1, pins: [] });
  assert.equal(pins.isPinned(f.childRepo), false);
});

test('a queued unpin follows an in-flight pin without reopening or closing the repository', async t => {
  const f = fixture(t);
  const pins = f.manager();
  t.after(() => pins.dispose());
  await Promise.all([pins.pin(f.root, [f.first], () => true), pins.unpin(f.childRepo)]);
  assert.equal(f.opens, 1);
  assert.deepEqual(f.saved.pins, []);
  assert.ok(f.git.repositories.includes(f.childRepo));
});

test('restoring an independently open repository does not claim ownership', async t => {
  const f = fixture(t, true);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  const pins = f.manager();
  t.after(() => pins.dispose());
  await waitFor(() => pins.isPinned(f.childRepo));
  assert.equal(f.opens, 0);
  await pins.unpin(f.childRepo);
  assert.ok(f.git.repositories.includes(f.childRepo));
  assert.equal(pins.isPinned(f.childRepo), false);
});

test('a stale durable chain is reported without opening a repository', async t => {
  const f = fixture(t);
  f.setValidation(async () => { throw Object.assign(new Error('Submodule identity changed'), { code: 'stale_target' }); });
  const pins = f.manager();
  t.after(() => pins.dispose());
  await assert.rejects(pins.pin(f.root, [f.first], () => true), /identity changed/i);
  assert.equal(f.opens, 0);
  assert.equal(f.saved, undefined);
});

test('workspace escape during repository opening prevents a pin', { skip: process.platform === 'win32' }, async t => {
  const f = fixture(t);
  const outside = path.join(path.dirname(f.root), 'outside-open');
  fs.mkdirSync(outside);
  let release, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const original = f.git.openRepository.bind(f.git);
  f.git.openRepository = async value => {
    entered();
    await new Promise(resolve => { release = resolve; });
    return original(value);
  };
  const pins = f.manager();
  t.after(() => pins.dispose());
  const pending = pins.pin(f.root, [f.first], () => true);
  await started;
  fs.renameSync(f.child, `${f.child}-saved`);
  fs.symlinkSync(outside, f.child);
  release();
  await assert.rejects(pending, /could not be opened|changed|outside/i);
  assert.equal(f.saved, undefined);
});

test('visible pins are revalidated and lose their marker when registration changes', async t => {
  const f = fixture(t);
  const pins = f.manager({ revalidateIntervalMs: 20, restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await pins.pin(f.root, [f.first], () => true);
  assert.equal(pins.isPinned(f.childRepo), true);
  f.setValidation(async () => { throw new Error('Submodule registration changed'); });
  await waitFor(() => !pins.isPinned(f.childRepo) && !!pins.error);
  assert.match(pins.error, /registration changed/i);
  assert.ok(f.git.repositories.includes(f.childRepo), 'invalidating a pin does not close or hide the repository');
  assert.equal(f.saved.pins.length, 1, 'the unavailable saved pin can be managed');
});

test('removes an unavailable saved pin from the explicit management picker', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  f.setValidation(async () => { throw new Error('Submodule unavailable'); });
  const pins = f.manager({ revalidateIntervalMs: 20, restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await waitFor(() => !!pins.error);
  f.setPick(choices => {
    assert.match(choices[0].detail, /unavailable/i);
    return choices[0];
  });
  await pins.manage();
  assert.deepEqual(f.saved.pins, []);
  assert.equal(pins.error, undefined);
});

test('pin management does not remove a choice after workspace scope changes', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  const pins = f.manager();
  t.after(() => pins.dispose());
  f.setPick(choices => { f.backend.generation++; return choices[0]; });
  await assert.rejects(pins.manage(), /workspace.*changed/i);
  assert.equal(f.saved.pins.length, 1);
});

test('slow pin revalidation coalesces changes and does not overlap', async t => {
  const f = fixture(t);
  const pins = f.manager({ revalidateIntervalMs: 20, restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await pins.pin(f.root, [f.first], () => true);
  let release, active = 0, maximum = 0;
  f.setValidation(async () => {
    active++;
    maximum = Math.max(maximum, active);
    await new Promise(resolve => { release = resolve; });
    active--;
    return { repos: [f.root, f.child] };
  });
  await waitFor(() => !!release);
  f.backend.generation++;
  f.tree.changed();
  f.tree.changed();
  assert.equal(maximum, 1);
  release();
  pins.setVisible(false);
  await waitFor(() => !pins.restoreQueued);
  assert.equal(maximum, 1);
});

test('removing a saved pin is not blocked by a stuck background validation', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  let release, calls = 0;
  f.setValidation(() => { calls++; return new Promise(resolve => { release = resolve; }); });
  const pins = f.manager({ revalidateIntervalMs: 1000, restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await waitFor(() => !!release);
  let timeout;
  try {
    await Promise.race([pins.manage(), new Promise((_, reject) => {
      timeout = setTimeout(() => reject(new Error('Pin removal waited for background validation')), 1000);
    })]);
  } finally { clearTimeout(timeout); }
  assert.deepEqual(f.saved.pins, []);
  assert.equal(f.opens, 0);
  release({ repos: [f.root, f.child] });
  await waitFor(() => !pins.restoreQueued);
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(calls, 1, 'no stale or hidden follow-up validation runs');
  assert.equal(pins.isPinned(f.childRepo), false);
});

test('backend-wide validation errors stop the sweep while individual stale pins do not', async t => {
  for (const global of [true, false]) {
    const f = fixture(t);
    f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }, { root: f.root, chain: [f.first, f.second] }] });
    let calls = 0;
    f.setValidation(async (_root, chain) => {
      calls++;
      if (chain.length === 1) throw Object.assign(new Error(global ? 'Backend unavailable' : 'Submodule changed'),
        { code: global ? 'service_exited' : 'stale_target' });
      return { repos: [f.root, f.child, f.nested] };
    });
    const pins = f.manager({ revalidateIntervalMs: 10000, restoreDelayMs: 0 });
    t.after(() => pins.dispose());
    await waitFor(() => !!pins.error && !pins.restoreQueued);
    assert.equal(calls, global ? 1 : 3);
    assert.equal(pins.isPinned(f.nestedRepo), !global);
  }
});

test('malformed saved pins can be reset only with confirmation and storage errors can be retried', async t => {
  const f = fixture(t);
  const malformed = { version: 99, pins: ['bad'] };
  f.setSaved(malformed);
  const pins = f.manager();
  t.after(() => pins.dispose());
  await pins.manage();
  assert.deepEqual(f.saved, malformed, 'dismissal keeps the original workspace state');
  f.setConfirm(() => 'Reset Pins');
  f.setFailure(new Error('workspace storage failed'));
  await assert.rejects(pins.manage(), /workspace storage failed/);
  assert.deepEqual(f.saved, malformed);
  f.setFailure(undefined);
  await pins.manage();
  assert.deepEqual(f.saved, { version: 1, pins: [] });
  assert.equal(pins.error, undefined);
  await pins.pin(f.root, [f.first], () => true);
  assert.equal(pins.isPinned(f.childRepo), true);
});

test('a sweep reuses one worker for multiple pins and closes it afterward', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }, { root: f.root, chain: [f.first, f.second] }] });
  let validations = 0;
  f.setValidation(async (_root, chain) => {
    validations++;
    return { repos: chain.length === 1 ? [f.root, f.child] : [f.root, f.child, f.nested] };
  });
  const pins = f.manager({ revalidateIntervalMs: 10000, restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await waitFor(() => pins.isPinned(f.childRepo) && pins.isPinned(f.nestedRepo) && !pins.restoreQueued);
  assert.equal(validations, 4);
  assert.equal(f.sessions, 1);
  assert.equal(f.sessionCloses, 1);
});

test('removing a pin fails fast until its already-dispatched VS Code open finishes', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  const originalOpen = f.git.openRepository.bind(f.git);
  let release, started;
  const opening = new Promise(resolve => { started = resolve; });
  f.git.openRepository = async value => {
    started();
    await new Promise(resolve => { release = resolve; });
    return originalOpen(value);
  };
  const pins = f.manager({ restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await opening;
  await assert.rejects(pins.manage(), /still opening.*try removing/i);
  assert.equal(f.saved.pins.length, 1);
  release();
  await waitFor(() => !pins.openings.size);
  await pins.manage();
  assert.deepEqual(f.saved.pins, []);
  assert.ok(f.git.repositories.includes(f.childRepo));
  await waitFor(() => !pins.restoreQueued);
  assert.equal(pins.isPinned(f.childRepo), false);
  assert.equal(f.opens, 1, 'no later open is dispatched after removal');
});

test('workspace changes during an already-dispatched open do not save or restore a pin', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }] });
  const originalOpen = f.git.openRepository.bind(f.git);
  let release, started;
  const opening = new Promise(resolve => { started = resolve; });
  f.git.openRepository = async value => {
    started();
    await new Promise(resolve => { release = resolve; });
    return originalOpen(value);
  };
  const pins = f.manager({ restoreDelayMs: 0 });
  t.after(() => pins.dispose());
  await opening;
  await assert.rejects(pins.manage(), /still opening/i);
  f.backend.generation++;
  f.setVisible(false);
  release();
  await waitFor(() => !pins.openings.size);
  await waitFor(() => !pins.restoreQueued);
  assert.equal(f.saved.pins.length, 1);
  assert.equal(pins.isPinned(f.childRepo), false);
  assert.ok(f.git.repositories.includes(f.childRepo), 'VS Code may complete an open that cannot be cancelled');
});

test('a never-settling open does not block other pins, actions, or worker cleanup', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }, { root: f.root, chain: [f.first, f.second] }] });
  const originalOpen = f.git.openRepository.bind(f.git);
  let started;
  const opening = new Promise(resolve => { started = resolve; });
  f.git.openRepository = async value => {
    if (value.fsPath === f.child) { started(); return new Promise(() => {}); }
    return originalOpen(value);
  };
  const pins = f.manager({ restoreDelayMs: 0, revalidateIntervalMs: 10000, openTimeoutMs: 10000 });
  t.after(() => pins.dispose());
  await opening;
  f.setPick(choices => choices.find(choice => choice.label === 'child'));
  await assert.rejects(pins.manage(), /still opening.*try removing/i);
  await waitFor(() => pins.isPinned(f.nestedRepo) && !pins.restoreQueued);
  assert.ok(f.sessionCloses >= 2, 'both the cancelled and follow-up validation workers close');
  f.setPick(choices => choices.find(choice => choice.label === 'nested'));
  await pins.manage();
  assert.equal(f.saved.pins.length, 1);
  assert.equal(f.saved.pins[0].chain.length, 1);
  pins.setVisible(false);
  const sessions = f.sessions;
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(f.sessions, sessions, 'hiding schedules no extra validation work');
  assert.equal(pins.openings.size, 1, 'the pending open remains tracked until VS Code finishes');
});

test('a timed-out open is skipped so a sweep can restore another pin', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, pins: [{ root: f.root, chain: [f.first] }, { root: f.root, chain: [f.first, f.second] }] });
  const originalOpen = f.git.openRepository.bind(f.git);
  f.git.openRepository = async value => value.fsPath === f.child ? new Promise(() => {}) : originalOpen(value);
  const pins = f.manager({ restoreDelayMs: 0, revalidateIntervalMs: 10000, openTimeoutMs: 20 });
  t.after(() => pins.dispose());
  await waitFor(() => pins.isPinned(f.nestedRepo) && !pins.restoreQueued);
  assert.match(pins.error, /still opening/i);
  assert.equal(f.sessions, 1);
  assert.equal(f.sessionCloses, 1);
});
