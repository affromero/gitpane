'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');

function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

async function fixture(t, worktree = false) {
  const root = path.resolve('workspace/repo'), parentRoot = path.resolve('workspace/parent');
  const uri = value => ({ fsPath: value, toString: () => value });
  const repo = { rootUri: uri(root), state: {} }, parent = { rootUri: uri(parentRoot), state: {} };
  const record = { group: 'working', path: 'child', kind: 'type_changed', original_path: null,
    conflict_stages: null, is_submodule: true };
  const page = changes => ({ snapshot: 'snapshot', revision: 'a'.repeat(64), cursor: 0,
    previous_cursor: null, next_cursor: null, changes });
  let rows = [record], token = 1, parentLinked = true, holdPage = false;
  const pageStarted = deferred(), releasePage = deferred(), verifyStarted = deferred(), releaseVerify = deferred();
  const searchStarted = deferred(), releaseSearch = deferred();
  const backend = {
    async beginChanges() {
      if (holdPage) { pageStarted.resolve(); await releasePage.promise; }
      return page(rows);
    },
    async verifyChanges() { verifyStarted.resolve(); await releaseVerify.promise; return { page: page([record]), change: record }; },
    async searchChanges() {
      searchStarted.resolve(); await releaseSearch.promise;
      return { snapshot: 'snapshot', revision: 'a'.repeat(64), count: 0, position: null, cursor: null, page: null, path: null, group: null };
    },
    async releaseChanges() {}
  };
  const tree = { backend, git: { repositories: worktree ? [repo, parent] : [repo] },
    onDidChangeTreeData: () => ({ dispose() {} }),
    setVisible() {},
    isCurrentRepository: (candidate, expected) => candidate.rootUri.fsPath === root ? token === expected : expected === 2,
    workspaceWorktrees: () => parentLinked ? [{ path: root }] : [],
    worktreeRepository: () => parentLinked ? repo : undefined };
  const load = Module._load;
  let GitpaneViewProvider;
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? {} : load.call(this, name, ...args); };
    ({ GitpaneViewProvider } = require('../src/extension'));
  } finally { Module._load = load; }
  const provider = new GitpaneViewProvider(tree, '/extension');
  t.after(() => provider.dispose());
  await provider.pager.refresh('owner', root, () => true);
  const node = { fileId: 'file', ownerId: 'owner', repo, repoToken: 1, group: record.group,
    change: { uri: uri(path.join(root, record.path)) }, record,
    ...(worktree ? { parentWorktree: { repoKey: parentRoot, repoToken: 2, path: root } } : {}) };
  provider.changes.set('file', node);
  return { provider, root, record, pageStarted, releasePage, verifyStarted, releaseVerify, searchStarted, releaseSearch,
    hold() { holdPage = true; }, setRows(value) { rows = value; }, reopen() { token++; }, unlink() { parentLinked = false; } };
}

test('revalidation waits for an overlapping pager refresh and rejects changed identities or rows', async t => {
  for (const outcome of ['unchanged', 'changed', 'removed', 'reopened', 'unlinked']) {
    const f = await fixture(t, outcome === 'unlinked');
    const checking = f.provider.revalidateChange('file');
    await f.verifyStarted.promise;
    f.hold();
    const refreshing = f.provider.pager.refresh('owner', f.root, () => true);
    await f.pageStarted.promise;
    f.releaseVerify.resolve();
    let settled = false;
    checking.then(() => { settled = true; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(settled, false, outcome);
    if (outcome === 'changed') f.setRows([{ ...f.record, kind: 'working_deleted' }]);
    if (outcome === 'removed') f.setRows([]);
    if (outcome === 'reopened') f.reopen();
    if (outcome === 'unlinked') f.unlink();
    f.releasePage.resolve();
    await refreshing;
    assert.equal(await checking, outcome === 'unchanged', outcome);
  }
});

test('revalidation follows a search replacing the pending refresh', async t => {
  const f = await fixture(t);
  const checking = f.provider.revalidateChange('file');
  await f.verifyStarted.promise;
  f.hold();
  const refreshing = f.provider.pager.refresh('owner', f.root, () => true);
  await f.pageStarted.promise;
  const searching = f.provider.pager.search('owner', 'missing');
  f.releaseVerify.resolve();
  f.releasePage.resolve();
  await f.searchStarted.promise;
  let settled = false;
  checking.then(() => { settled = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(settled, false);
  f.releaseSearch.resolve();
  await Promise.all([refreshing, searching]);
  assert.equal(await checking, true);
});

test('revalidation waits for a refresh already in progress before checking Git', async t => {
  const f = await fixture(t);
  f.hold();
  const refreshing = f.provider.pager.refresh('owner', f.root, () => true);
  await f.pageStarted.promise;
  const checking = f.provider.revalidateChange('file');
  let started = false;
  f.verifyStarted.promise.then(() => { started = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(started, false);
  f.releasePage.resolve();
  await refreshing;
  await f.verifyStarted.promise;
  f.releaseVerify.resolve();
  assert.equal(await checking, true);
});

test('diff dispatch waits when refresh begins after the last asynchronous selection check', async t => {
  for (const changed of [false, true]) {
    const f = await fixture(t);
    f.hold();
    const original = f.provider.waitForCurrentChange.bind(f.provider);
    let queued = false, refreshing;
    f.provider.waitForCurrentChange = async (...args) => {
      const result = await original(...args);
      if (!queued) {
        queued = true;
        queueMicrotask(() => { refreshing = f.provider.pager.refresh('owner', f.root, () => true); });
      }
      return result;
    };
    let dispatched = 0;
    const opening = f.provider.withCurrentChange(f.provider.changes.get('file'), () => { dispatched++; });
    await f.pageStarted.promise;
    assert.equal(dispatched, 0);
    if (changed) f.setRows([]);
    f.releasePage.resolve();
    await refreshing;
    assert.equal(await opening, !changed);
    assert.equal(dispatched, changed ? 0 : 1);
  }
});
