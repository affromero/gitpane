'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { Status, canonicalPath, comparison, groups, isSubmodule, parseDiscovery, visibleRepository } = require('../src/model');

test('changed files retain their staged and unstaged groups', () => {
  const staged = { status: Status.INDEX_MODIFIED, uri: 'file' };
  const modified = { status: Status.MODIFIED, uri: 'file' };
  const added = { status: Status.UNTRACKED, uri: 'new' };
  const result = groups({ indexChanges: [staged], workingTreeChanges: [modified, added] });
  assert.deepEqual(result.map(group => [group.label, group.changes.map(c => c.uri)]), [
    ['Staged', ['file']], ['Unstaged', ['file']], ['Untracked', ['new']]
  ]);
});

test('comparisons preserve staged, working, rename, addition and deletion sides', () => {
  assert.deepEqual(comparison('index', { status: Status.INDEX_RENAMED, uri: 'new', originalUri: 'old' }),
    { left: { git: 'old', ref: 'HEAD' }, right: { git: 'new', ref: '' } });
  assert.deepEqual(comparison('index', { status: Status.INDEX_ADDED, uri: 'new' }),
    { left: { empty: true }, right: { git: 'new', ref: '' } });
  assert.deepEqual(comparison('index', { status: Status.INDEX_DELETED, uri: 'old' }),
    { left: { git: 'old', ref: 'HEAD' }, right: { empty: true } });
  assert.deepEqual(comparison('working', { status: Status.MODIFIED, uri: 'file' }),
    { left: { git: 'file', ref: '' }, right: { file: 'file' } });
  assert.deepEqual(comparison('working', { status: Status.DELETED, uri: 'file' }),
    { left: { git: 'file', ref: '' }, right: { empty: true } });
  assert.deepEqual(comparison('untracked', { status: Status.UNTRACKED, uri: 'new' }),
    { left: { empty: true }, right: { file: 'new' } });
  assert.deepEqual(comparison('merge', { uri: 'conflict' }), { open: 'conflict' });
});

test('discovery accepts absolute paths and rejects malformed or incompatible output', () => {
  const repo = path.resolve('repo with spaces');
  assert.deepEqual(parseDiscovery(JSON.stringify({ version: 1, repositories: [repo, repo] }), path.isAbsolute), [repo]);
  for (const output of ['not json', '{"version":2,"repositories":[]}', '{"version":1,"repositories":["relative"]}']) {
    assert.throws(() => parseDiscovery(output, path.isAbsolute));
  }
});

test('repository visibility follows discovery, linked worktrees and workspace boundaries', () => {
  const base = path.resolve('workspace');
  const discovered = new Set([path.join(base, 'discovered')]);
  const worktrees = new Set([path.join(base, 'worktree')]);
  const workspace = [path.join(base, 'open')];
  assert.equal(visibleRepository(path.join(base, 'discovered'), discovered, worktrees, workspace), true);
  assert.equal(visibleRepository(path.join(base, 'worktree'), discovered, worktrees, workspace), true);
  assert.equal(visibleRepository(path.join(base, 'open', 'nested'), discovered, worktrees, workspace), true);
  assert.equal(visibleRepository(base, discovered, worktrees, workspace), true);
  assert.equal(visibleRepository(path.join(base, 'discovered-other'), discovered, worktrees, workspace), false);
  assert.equal(visibleRepository(path.join(base, 'removed'), discovered, worktrees, workspace), false);
});

test('submodule detection targets the submodule entry, not its neighboring files', () => {
  const root = path.resolve('repo');
  const modules = [{ path: 'vendor/module' }];
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module'), modules), true);
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module-other'), modules), false);
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module/file'), modules), false);
});

test('repository aliases resolve to the same discovered checkout', t => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-alias-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const repo = path.join(temporary, 'repo');
  const alias = path.join(temporary, 'alias');
  fs.mkdirSync(repo);
  try { fs.symlinkSync(repo, alias, 'junction'); }
  catch (error) {
    if (process.platform !== 'win32') throw error;
    t.skip(`Could not create directory junction: ${error.message}`);
    return;
  }
  assert.equal(visibleRepository(alias, new Set([canonicalPath(repo)]), new Set(), []), true);
});
