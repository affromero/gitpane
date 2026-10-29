'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { WorkspaceRepositories, readRepositories } = require('../src/workspace-repositories');

async function waitFor(predicate) {
  const deadline = Date.now() + 3000;
  while (!predicate()) { if (Date.now() > deadline) throw new Error('Repository restore did not settle'); await new Promise(resolve => setTimeout(resolve, 10)); }
}

function fixture(t) {
  const workspace = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-added-')));
  t.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  const root = path.join(workspace, 'project', 'repo');
  fs.mkdirSync(root, { recursive: true });
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => `file:${value}` });
  const repository = { rootUri: uri(root) };
  let saved, failure, selection = [uri(root)], pick = choices => choices[0], warning = () => undefined;
  let openCalls = 0, validIdentity = true, beforeOpen = async () => {}, afterSave = () => {};
  const listeners = new Set(), tokens = new Map(), revisions = new Map();
  const state = { get: () => saved, async update(_key, value) {
    if (failure) throw failure;
    saved = structuredClone(value); await afterSave(value);
  } };
  const backend = { generation: 1, async previewRepository(value) {
    if (value !== root) throw Object.assign(new Error('not a repository root'), { code: 'repository_error' });
    return { repo_identity: '1'.repeat(64) };
  }, async validateRepositoryIdentity() { return { valid: validIdentity }; } };
  const tree = { backendGeneration: 1, backendStatuses: new Map(),
    onDidChangeTreeData(handler) { listeners.add(handler); return { dispose: () => listeners.delete(handler) }; },
    changed() { for (const handler of listeners) handler(); },
    directoryIdentity(value) { try { const real = fs.realpathSync.native(value), stat = fs.statSync(real, { bigint: true });
      return { path: process.platform === 'win32' ? real.toLowerCase() : real, dev: stat.dev, ino: stat.ino }; } catch { return undefined; } },
    isPathInWorkspace(value) { const real = this.directoryIdentity(value)?.path; return !!real && (real === workspace || real.startsWith(workspace + path.sep)); },
    repositoryToken: repo => tokens.get(repo?.rootUri.toString()), repositoryRevision: value => revisions.get(value.toString()) || 0,
    isCurrentRepository: (repo, token) => !!repo && token !== undefined && tokens.get(repo.rootUri.toString()) === token && git.repositories.includes(repo) };
  const git = { repositories: [] };
  const pending = new Set();
  const openings = { hasOpening: value => pending.has(value), async openTarget(target, targetUri, guard) {
    if (pending.has(target)) throw Object.assign(new Error('still opening'), { code: 'open_pending' });
    pending.add(target);
    try {
      openCalls++; await beforeOpen();
      if (!guard()) throw new Error('workspace changed');
      git.repositories.push(repository);
      tokens.set(targetUri.toString(), (tokens.get(targetUri.toString()) || 0) + 1);
      revisions.set(targetUri.toString(), (revisions.get(targetUri.toString()) || 0) + 1);
      tree.backendStatuses.set(targetUri.toString(), { repo_identity: '1'.repeat(64) });
      tree.changed();
      return repository;
    } finally { pending.delete(target); }
  } };
  const vscode = { Uri: { file: uri }, window: { showOpenDialog: async () => selection, showQuickPick: async choices => pick(choices),
    showInformationMessage: async () => {}, showWarningMessage: async () => warning() } };
  const manager = options => new WorkspaceRepositories(tree, backend, git, state, vscode, openings, options);
  return { root, workspace, repository, tree, backend, git, openings, manager, get saved() { return saved; }, get opens() { return openCalls; },
    select(value) { selection = value; }, setSaved(value) { saved = value; }, setFailure(value) { failure = value; },
    setIdentity(value) { validIdentity = value; }, setPick(value) { pick = value; }, setWarning(value) { warning = value; },
    beforeOpen(value) { beforeOpen = value; }, afterSave(value) { afterSave = value; },
    close() { git.repositories = []; tokens.clear(); tree.changed(); },
    register(repo) {
      git.repositories.push(repo);
      const key = repo.rootUri.toString();
      tokens.set(key, (tokens.get(key) || 0) + 1); revisions.set(key, (revisions.get(key) || 0) + 1);
      tree.backendStatuses.set(key, { repo_identity: '1'.repeat(64) }); tree.changed();
      return repo;
    } };
}

test('explicitly added repositories persist, restore and can stop restoring without closing', async t => {
  const f = fixture(t), first = f.manager();
  await first.add();
  assert.equal(f.opens, 1);
  assert.equal(first.isAdded(f.repository), true);
  assert.equal(f.saved.repositories.length, 1);
  await first.add();
  assert.equal(f.opens, 1);
  first.dispose(); f.close();
  const restored = f.manager();
  t.after(() => restored.dispose());
  restored.setVisible(true);
  await waitFor(() => f.git.repositories.includes(f.repository));
  await restored.remove(f.repository);
  assert.deepEqual(f.saved.repositories, []);
  assert.ok(f.git.repositories.includes(f.repository));
});

test('cancel, outside paths, invalid roots and changed workspace do not save additions', async t => {
  const f = fixture(t), manager = f.manager();
  t.after(() => manager.dispose());
  f.select(undefined);
  await manager.add();
  f.select([{ scheme: 'file', fsPath: path.dirname(f.workspace) }]);
  await assert.rejects(manager.add(), /inside.*workspace/i);
  f.select([{ scheme: 'file', fsPath: f.workspace }]);
  await assert.rejects(manager.add(), /not a repository root/i);
  f.select([{ scheme: 'file', fsPath: f.root }]);
  f.beforeOpen(async () => { f.backend.generation++; });
  await assert.rejects(manager.add(), /changed/i);
  assert.equal(f.saved, undefined);
  assert.equal(f.git.repositories.length, 0);
});

test('identity replacement stops restoration without closing an independently open repository', async t => {
  const f = fixture(t), first = f.manager();
  await first.add(); first.dispose(); f.close();
  f.setIdentity(false);
  const restored = f.manager(); t.after(() => restored.dispose());
  restored.setVisible(true);
  await waitFor(() => !!restored.error);
  assert.match(restored.error, /changed/i);
  assert.equal(f.git.repositories.length, 0);
  await restored.manage();
  assert.deepEqual(f.saved.repositories, []);
});

test('retargeting the selected folder during validation prevents opening', { skip: process.platform === 'win32' }, async t => {
  const f = fixture(t), manager = f.manager();
  t.after(() => manager.dispose());
  const alias = path.join(f.workspace, 'alias'), other = path.join(f.workspace, 'other');
  fs.mkdirSync(other); fs.symlinkSync(f.root, alias);
  f.select([{ scheme: 'file', fsPath: alias }]);
  let release;
  f.backend.previewRepository = async () => { await new Promise(resolve => { release = resolve; }); return { repo_identity: '1'.repeat(64) }; };
  const pending = manager.add();
  await waitFor(() => !!release);
  fs.unlinkSync(alias); fs.symlinkSync(other, alias); release();
  await assert.rejects(pending, /changed/i);
  assert.equal(f.opens, 0);
  assert.equal(f.saved, undefined);
});

test('a saved addition cannot be removed while its VS Code open is pending', async t => {
  const f = fixture(t), first = f.manager();
  await first.add(); first.dispose(); f.close();
  let release;
  f.beforeOpen(() => new Promise(resolve => { release = resolve; }));
  const manager = f.manager(); t.after(() => manager.dispose());
  manager.setVisible(true);
  await waitFor(() => !!release);
  await assert.rejects(manager.manage(), /still opening/i);
  assert.equal(f.saved.repositories.length, 1);
  release();
  await waitFor(() => !f.openings.hasOpening(f.root));
  await manager.manage();
  assert.deepEqual(f.saved.repositories, []);
});

test('a failed VS Code open does not block other saved repositories and retries while visible', async t => {
  for (const failure of ['reject', 'null']) {
    const f = fixture(t);
    const second = path.join(f.workspace, 'other');
    fs.mkdirSync(second);
    const uri = value => ({ scheme: 'file', fsPath: value, toString: () => `file:${value}` });
    const other = { rootUri: uri(second) };
    const entry = source => { const identity = f.tree.directoryIdentity(source); return { source, path: identity.path,
      dev: String(identity.dev), ino: String(identity.ino), repo_identity: '1'.repeat(64) }; };
    f.setSaved({ version: 1, repositories: [entry(f.root), entry(second)] });
    let attempts = 0;
    f.openings.openTarget = async target => {
      if (target === f.root && ++attempts === 1) {
        if (failure === 'reject') throw new Error('open failed');
        return undefined;
      }
      return f.register(target === f.root ? f.repository : other);
    };
    const manager = f.manager({ retryDelayMs: 20, restoreDelayMs: 0 });
    t.after(() => manager.dispose());
    manager.setVisible(true);
    await waitFor(() => f.git.repositories.includes(other));
    assert.ok(manager.error, 'the first failed open is reported');
    await waitFor(() => f.git.repositories.includes(f.repository) && !manager.error);
    assert.ok(attempts >= 2);
    manager.setVisible(false);
  }
});

test('storage failures and malformed state are reported and require explicit reset', async t => {
  const f = fixture(t);
  f.setSaved({ version: 2, repositories: [] });
  assert.throws(() => readRepositories(f.saved), /invalid/i);
  const manager = f.manager(); t.after(() => manager.dispose());
  await manager.manage();
  assert.equal(f.saved.version, 2);
  f.setWarning(() => 'Reset Workspace Repositories');
  f.setFailure(new Error('storage failed'));
  await assert.rejects(manager.manage(), /storage failed/i);
  f.setFailure(undefined);
  await manager.manage();
  assert.deepEqual(f.saved.repositories, []);
});
