'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { HiddenRepositories, readHidden } = require('../src/hidden-repositories');

function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-hidden-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const first = path.join(root, 'one', 'same');
  const second = path.join(root, 'two', 'same');
  fs.mkdirSync(first, { recursive: true });
  fs.mkdirSync(second, { recursive: true });
  let saved, failure, onSave = () => {}, pick = choices => choices[0], confirm = () => undefined;
  const state = { get: () => saved, async update(_key, value) {
    if (failure) throw failure;
    saved = structuredClone(value);
    await onSave(value);
  } };
  const backend = { generation: 1 };
  const tree = { directoryIdentity(value) {
    try { const real = fs.realpathSync.native(value), stat = fs.statSync(real, { bigint: true });
      return { path: process.platform === 'win32' ? real.toLowerCase() : real, dev: stat.dev, ino: stat.ino }; }
    catch { return undefined; }
  }, isPathInWorkspace: value => value.startsWith(root + path.sep), changed() {} };
  const vscode = { window: { showQuickPick: async choices => pick(choices), showInformationMessage: async () => {},
    showWarningMessage: async () => confirm() } };
  const manager = () => new HiddenRepositories(tree, backend, state, vscode);
  return { first, second, tree, backend, manager, state, get saved() { return saved; },
    setSaved(value) { saved = value; }, setPick(value) { pick = value; }, setConfirm(value) { confirm = value; },
    setFailure(value) { failure = value; }, onSave(value) { onSave = value; } };
}

const identity = '1'.repeat(64);

test('hidden repositories persist exact identities and restore individually across reload', async t => {
  const f = fixture(t);
  const first = f.manager();
  await first.hide(f.first, identity, () => true);
  assert.equal(first.isHidden(f.tree.directoryIdentity(f.first), identity), true);
  assert.equal(first.isHidden(f.tree.directoryIdentity(f.second), identity), false);
  assert.equal(first.isHidden(f.tree.directoryIdentity(f.first), '2'.repeat(64)), false);
  first.dispose();
  const restored = f.manager();
  t.after(() => restored.dispose());
  assert.equal(restored.isHidden(f.tree.directoryIdentity(f.first), identity), true);
  assert.equal(restored.isHidden(f.tree.directoryIdentity(f.first), undefined), true, 'startup waits for identity verification');
  assert.equal(restored.isHidden(f.tree.directoryIdentity(f.first), '2'.repeat(64)), false, 'a verified replacement becomes visible');
  await restored.manage();
  assert.equal(restored.isHidden(f.tree.directoryIdentity(f.first), identity), false);
  assert.deepEqual(f.saved.repositories, []);
});

test('a replaced directory or repository identity is not silently hidden', async t => {
  const f = fixture(t);
  const hidden = f.manager();
  t.after(() => hidden.dispose());
  await hidden.hide(f.first, identity, () => true);
  fs.renameSync(f.first, `${f.first}-old`);
  fs.mkdirSync(f.first);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), identity), false);
  fs.rmdirSync(f.first);
  fs.renameSync(`${f.first}-old`, f.first);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), identity), true);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), '2'.repeat(64)), false);
});

test('workspace changes and storage errors cannot publish a hidden selection', async t => {
  const f = fixture(t);
  const hidden = f.manager();
  t.after(() => hidden.dispose());
  f.setFailure(new Error('storage failed'));
  await assert.rejects(hidden.hide(f.first, identity, () => true), /storage failed/);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), identity), false);
  f.setFailure(undefined);
  let current = true;
  f.onSave(value => { if (value.repositories.length) current = false; });
  await assert.rejects(hidden.hide(f.first, identity, () => current), /changed/);
  assert.deepEqual(f.saved.repositories, []);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), identity), false);
});

test('stale and forged restore choices leave saved visibility unchanged', async t => {
  const f = fixture(t);
  const hidden = f.manager();
  t.after(() => hidden.dispose());
  await hidden.hide(f.first, identity, () => true);
  f.setPick(choices => ({ ...choices[0] }));
  await assert.rejects(hidden.manage(), /changed/);
  f.setPick(choices => { f.backend.generation++; return choices[0]; });
  await assert.rejects(hidden.manage(), /workspace changed/i);
  assert.equal(hidden.isHidden(f.tree.directoryIdentity(f.first), identity), true);
});

test('malformed state is reported and only reset after explicit confirmation', async t => {
  const f = fixture(t);
  f.setSaved({ version: 1, repositories: [{ path: '/broken' }] });
  assert.throws(() => readHidden(f.saved), /invalid/i);
  const hidden = f.manager();
  t.after(() => hidden.dispose());
  assert.match(hidden.error, /invalid/i);
  await hidden.manage();
  assert.match(hidden.error, /invalid/i);
  f.setConfirm(() => 'Reset Hidden Repositories');
  f.setFailure(new Error('storage failed'));
  await assert.rejects(hidden.manage(), /storage failed/);
  f.setFailure(undefined);
  await hidden.manage();
  assert.equal(hidden.error, undefined);
  assert.deepEqual(f.saved.repositories, []);
});
