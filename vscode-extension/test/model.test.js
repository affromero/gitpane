'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { spawnSync } = require('node:child_process');
const { isolatedGitEnv } = require('./integration/git-env');
const { Status, canonicalPath, changedFileCount, comparison, groups, isSubmodule, visibleRepository } = require('../src/model');

test('changed files retain their staged and unstaged groups', () => {
  const staged = { status: Status.INDEX_MODIFIED, uri: 'file' };
  const modified = { status: Status.MODIFIED, uri: 'file' };
  const added = { status: Status.UNTRACKED, uri: 'new' };
  const result = groups({ indexChanges: [staged], workingTreeChanges: [modified, added] });
  assert.deepEqual(result.map(group => [group.label, group.changes.map(c => c.uri)]), [
    ['Staged', ['file']], ['Unstaged', ['file']], ['Untracked', ['new']]
  ]);
  assert.equal(changedFileCount(result), 2);
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
});

test('conflict diffs compare available index stages even without a working file', () => {
  const cases = [
    [Status.ADDED_BY_US, null, ':2'],
    [Status.ADDED_BY_THEM, null, ':3'],
    [Status.DELETED_BY_US, ':1', ':3'],
    [Status.DELETED_BY_THEM, ':1', ':2'],
    [Status.BOTH_ADDED, ':2', ':3'],
    [Status.BOTH_DELETED, ':1', null],
    [Status.BOTH_MODIFIED, ':2', ':3']
  ];
  const side = ref => ref ? { git: 'conflict.txt', ref } : { empty: true };
  for (const [status, left, right] of cases) {
    assert.deepEqual(comparison('merge', { status, uri: 'conflict.txt' }),
      { left: side(left), right: side(right) }, `conflict status ${status}`);
  }
  assert.throws(() => comparison('merge', { status: -1, uri: 'conflict.txt' }), /unsupported conflict status/i);
});

test('conflict comparisons resolve real Git index contents when the working file is missing', t => {
  const version = spawnSync('git', ['--version'], { encoding: 'utf8' });
  if (version.error?.code === 'ENOENT') return t.skip('Git is not installed');
  assert.ifError(version.error);
  assert.equal(version.status, 0, version.stderr);
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-conflict-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const env = isolatedGitEnv();
  const git = (args, input) => {
    const result = spawnSync('git', ['-C', temporary, ...args], { env, input, encoding: 'utf8' });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout;
  };
  git(['init', '-q']);
  const contents = { 1: 'base\n', 2: 'ours\n', 3: 'theirs\n' };
  const objects = Object.fromEntries(Object.entries(contents).map(([stage, value]) =>
    [stage, git(['hash-object', '-w', '--stdin'], value).trim()]));
  const cases = [
    [Status.ADDED_BY_US, [2], '', contents[2]],
    [Status.ADDED_BY_THEM, [3], '', contents[3]],
    [Status.DELETED_BY_US, [1, 3], contents[1], contents[3]],
    [Status.DELETED_BY_THEM, [1, 2], contents[1], contents[2]],
    [Status.BOTH_ADDED, [2, 3], contents[2], contents[3]],
    [Status.BOTH_DELETED, [1], contents[1], ''],
    [Status.BOTH_MODIFIED, [1, 2, 3], contents[2], contents[3]]
  ];
  for (const [status, stages, expectedLeft, expectedRight] of cases) {
    const file = `conflict-${status}.txt`;
    const entries = stages.map(stage => `100644 ${objects[stage]} ${stage}\t${file}\n`).join('');
    git(['update-index', '--index-info'], entries);
    assert.equal(fs.existsSync(path.join(temporary, file)), false);
    const plan = comparison('merge', { status, uri: file });
    const read = side => side.empty ? '' : git(['show', `${side.ref}:${side.git}`]);
    assert.equal(read(plan.left), expectedLeft, `left side for conflict status ${status}`);
    assert.equal(read(plan.right), expectedRight, `right side for conflict status ${status}`);
  }
});

test('repository visibility stays within workspace boundaries', () => {
  const base = path.resolve('workspace');
  const workspace = [path.join(base, 'open')];
  assert.equal(visibleRepository(path.join(base, 'open'), workspace), true);
  assert.equal(visibleRepository(path.join(base, 'open', 'nested'), workspace), true);
  assert.equal(visibleRepository(base, workspace), false);
  assert.equal(visibleRepository(path.join(base, 'open-other'), workspace), false);
  assert.equal(visibleRepository(path.join(base, 'removed'), workspace), false);
});

test('submodule detection targets the submodule entry, not its neighboring files', () => {
  const root = path.resolve('repo');
  const modules = [{ path: 'vendor/module' }];
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module'), modules), true);
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module-other'), modules), false);
  assert.equal(isSubmodule(root, path.join(root, 'vendor/module/file'), modules), false);
});

test('repository aliases resolve to the same workspace checkout', t => {
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
  assert.equal(visibleRepository(alias, [canonicalPath(repo)]), true);
});
