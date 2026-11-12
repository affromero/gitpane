'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { SubmoduleDiscovery } = require('../src/submodule-discovery');

test('opting out cancels pending discovery without closing already open repositories', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-discovery-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const child = path.join(root, 'child');
  fs.mkdirSync(child);
  fs.writeFileSync(path.join(child, '.git'), 'gitdir: ../modules/child\n');
  const uri = value => ({ fsPath: value, toString: () => value });
  const parent = { rootUri: uri(root), state: { submodules: [{ path: 'child' }] } };
  const opened = [];
  let resolveInspection;
  const status = { repo_identity: 'a'.repeat(64) };
  const identity = value => {
    try { const stat = fs.statSync(value, { bigint: true }); return { path: fs.realpathSync.native(value), dev: stat.dev, ino: stat.ino }; }
    catch { return undefined; }
  };
  const record = { key: root, repo: parent, status, identity: identity(root) };
  const tree = { disposed: false, backendGeneration: 1, backendStatuses: new Map([[root, status]]),
    git: { repositories: [parent], async openRepository(value) { opened.push(value.fsPath); return undefined; } },
    backend: { discoverSubmodule: () => new Promise(resolve => { resolveInspection = resolve; }),
      async validateSubmodulePin(parentPath, chain) { return { repos: [parentPath, path.join(parentPath, chain[0].path)] }; } },
    presentationView: () => ({ records: [record] }), directoryIdentity: identity,
    repositoryToken: () => 1, contentRevision: () => 0, isCurrentRepository: () => true,
    isPathInWorkspace: value => value.startsWith(`${identity(root).path}${path.sep}`) };
  const discovery = new SubmoduleDiscovery(tree, { Uri: { file: uri } });
  const pending = discovery.refresh();
  discovery.configure(false);
  resolveInspection({ parent_identity: status.repo_identity, child_identity: 'b'.repeat(64), repo: child });
  await pending;
  assert.deepEqual(opened, [], 'a stale inspection cannot open a repository after opt-out');
  await discovery.refresh();
  assert.deepEqual(opened, [], 'disabled discovery does not open repositories');
  discovery.configure(true);
  tree.backend.discoverSubmodule = async () => ({ parent_identity: status.repo_identity, child_identity: 'b'.repeat(64), repo: child });
  await discovery.refresh();
  assert.deepEqual(opened, [identity(child).path], 'enabled discovery opens an initialized verified child');
  let hidden = false;
  let finishValidation;
  tree.hiddenRepositories = { isHidden: () => hidden };
  tree.backend.validateSubmodulePin = parentPath => new Promise(resolve => {
    finishValidation = () => resolve({ repos: [parentPath, identity(child).path] });
  });
  const validating = discovery.refresh();
  await Promise.resolve();
  hidden = true;
  finishValidation();
  await validating;
  assert.deepEqual(opened, [identity(child).path], 'hiding a child during validation prevents another automatic open');
  discovery.configure(false);
  assert.deepEqual(opened, [identity(child).path], 'opting out does not close an opened repository');
});
