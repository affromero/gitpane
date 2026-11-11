'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { createStashSource, normalizeContent, normalizeList } = require('../src/stash-source');

const repo = path.resolve('workspace/repo');
const identity = 'b'.repeat(64);
const oid = 'a'.repeat(40);
const parent = 'c'.repeat(40);
const untracked = 'd'.repeat(40);

const rawFile = (overrides = {}) => ({ source: 'tracked', status: 'M', path: 'src/file.txt',
  old_path: 'src/file.txt', new_path: 'src/file.txt', is_binary: false, is_submodule: false, ...overrides });
const rawList = (overrides = {}) => ({ oid, message: 'WIP on main', parent_oid: parent,
  untracked_parent_oid: untracked, files: [rawFile()], repo_identity: identity, ...overrides });
const side = (kind, text, extra = {}) => ({ kind, oid: kind === 'missing' ? null : 'e'.repeat(40),
  mode: kind === 'missing' ? null : 33188, size: text === null ? 0 : text.length, text, ...extra });

function fixture(options = {}) {
  const calls = [];
  const rawContent = (overrides = {}) => ({ oid, file: rawFile(), old: side('text', 'old\n'), new: side('text', 'new\n'),
    diff: '@@ -1 +1 @@\n-old\n+new\n', repo_identity: identity, ...overrides });
  const backend = {
    async stashFiles(repoPath, value) { calls.push(['stash_files', repoPath, value]); return options.list ?? rawList(); },
    async stashFileContent(repoPath, value, expected, source, filePath) {
      calls.push(['stash_file_content', repoPath, value, expected, source, filePath]);
      return options.content ?? rawContent({ file: rawFile({ source, path: filePath }) });
    }
  };
  return { source: createStashSource(backend), backend, calls, rawContent };
}

test('stash file lists use the full OID and the expected repository identity', async () => {
  const f = fixture();
  const list = await f.source.listFiles({ repo, identity, oid });
  assert.deepEqual(f.calls, [['stash_files', repo, oid]]);
  assert.equal(list.stash_oid, oid);
  assert.equal(list.repo_identity, identity);
  assert.equal(list.parent_oid, parent);
  assert.equal(list.untracked_parent_oid, untracked);
  assert.deepEqual(list.files, [{ source: 'tracked', status: 'M', path: 'src/file.txt',
    old_path: 'src/file.txt', new_path: 'src/file.txt', binary: false, gitlink: false }]);
});

test('saved-untracked and submodule entries keep their source and pointer flags', async () => {
  const f = fixture({ list: rawList({ files: [
    rawFile({ source: 'saved_untracked', status: 'A', path: 'notes with space.txt', old_path: null, new_path: 'notes with space.txt' }),
    rawFile({ source: 'tracked', status: 'D', path: 'gone.txt', old_path: 'gone.txt', new_path: null }),
    rawFile({ path: 'child', old_path: 'child', new_path: 'child', is_submodule: true }),
    rawFile({ status: 'R', path: 'new name.txt', old_path: 'old name.txt', new_path: 'new name.txt' })
  ] }) });
  const list = await f.source.listFiles({ repo, identity, oid });
  assert.deepEqual(list.files.map(file => [file.source, file.status, file.path, file.old_path, file.new_path, file.gitlink]), [
    ['saved-untracked', 'A', 'notes with space.txt', null, 'notes with space.txt', false],
    ['tracked', 'D', 'gone.txt', 'gone.txt', null, false],
    ['tracked', 'M', 'child', 'child', 'child', true],
    ['tracked', 'R', 'new name.txt', 'old name.txt', 'new name.txt', false]
  ]);
  await f.source.readFile({ repo, identity, oid, file: list.files[0] });
  assert.deepEqual(f.calls.at(-1), ['stash_file_content', repo, oid, identity, 'saved_untracked', 'notes with space.txt']);
  await f.source.readFile({ repo, identity, oid, file: list.files[1] });
  assert.deepEqual(f.calls.at(-1), ['stash_file_content', repo, oid, identity, 'tracked', 'gone.txt']);
});

test('content responses keep saved text, empty missing sides and undecoded binaries', async () => {
  const f = fixture();
  const file = normalizeList(rawList(), oid, identity).files[0];
  const content = await f.source.readFile({ repo, identity, oid, file });
  assert.deepEqual(content, { old: { kind: 'text', text: 'old\n' }, new: { kind: 'text', text: 'new\n' } });

  const missingSide = fixture({ content: { oid, file: rawFile(), old: side('missing', ''),
    new: side('binary', null), diff: '', repo_identity: identity } });
  const mixed = await missingSide.source.readFile({ repo, identity, oid, file });
  assert.deepEqual(mixed, { old: { kind: 'missing', text: '' }, new: { kind: 'binary', text: null } });

  const pointer = fixture({ content: { oid, file: rawFile({ is_submodule: true }), old: side('gitlink', 'Subproject commit ' + '1'.repeat(40) + '\n'),
    new: side('gitlink', 'Subproject commit ' + '2'.repeat(40) + '\n'), diff: '', repo_identity: identity } });
  assert.deepEqual(await pointer.source.readFile({ repo, identity, oid, file }), {
    old: { kind: 'gitlink', text: `Subproject commit ${'1'.repeat(40)}\n` },
    new: { kind: 'gitlink', text: `Subproject commit ${'2'.repeat(40)}\n` } });
});

test('stale identities, renumbered OIDs and cross-wired files are rejected', async () => {
  const otherOid = 'f'.repeat(40);
  assert.throws(() => normalizeList(rawList({ oid: otherOid }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ repo_identity: 'c'.repeat(64) }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ parent_oid: null }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ files: [rawFile({ status: 'X' })] }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ files: [rawFile({ path: '/abs' })] }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ files: [rawFile({ source: 'index' })] }), oid, identity), /invalid stash file list/);
  assert.throws(() => normalizeList(rawList({ files: [rawFile({ status: 'R', old_path: null })] }), oid, identity),
    /invalid stash file list/);
  assert.throws(() => normalizeContent({ oid, file: rawFile({ path: 'other.txt' }), old: side('text', 'a'),
    new: side('text', 'b'), diff: '', repo_identity: identity }, oid, identity, 'tracked', rawFile()),
    /invalid stash file contents/);
  assert.throws(() => normalizeContent({ oid, file: rawFile({ source: 'saved_untracked' }), old: side('text', 'a'),
    new: side('text', 'b'), diff: '', repo_identity: identity }, oid, identity, 'tracked', rawFile()),
    /invalid stash file contents/);
  assert.throws(() => normalizeContent({ oid, file: rawFile(), old: side('text', null),
    new: side('text', 'b'), diff: '', repo_identity: identity }, oid, identity, 'tracked', rawFile()),
    /invalid stash file contents/);
});

test('backends without stash reads report an unsupported backend', async () => {
  const source = createStashSource({});
  await assert.rejects(source.listFiles({ repo, identity, oid }), error => error.code === 'unsupported_backend' &&
    /does not support stash previews/.test(error.message));
  await assert.rejects(source.readFile({ repo, identity, oid, file: rawFile() }), error => error.code === 'unsupported_backend');
});
