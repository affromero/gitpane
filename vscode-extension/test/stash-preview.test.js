'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { openStashDiff, stashFileLabel, stashSides, validateStashContent, validateStashList } = require('../src/stash-preview');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

// Mirrors VS Code's Uri shape: `with` replaces components and the virtual
// providers see an opaque path, never the repository path or the file name.
function uri(value, scheme = 'file', component = value) {
  return {
    scheme, fsPath: component, path: component, query: '', fragment: '',
    toString() { return `${scheme}:${component}`; },
    with(change) { return uri(value, change.scheme ?? scheme, change.path ?? component); }
  };
}

const identity = 'b'.repeat(64);
const oid = 'a'.repeat(40);
const text = value => ({ kind: 'text', text: value });
const missing = () => ({ kind: 'missing', text: '' });
const binary = () => ({ kind: 'binary', text: null });
const gitlink = value => ({ kind: 'gitlink', text: `Subproject commit ${value}\n` });

function file(overrides = {}) {
  return { source: 'tracked', status: 'M', path: 'src/file.txt', old_path: 'src/file.txt', new_path: 'src/file.txt',
    binary: false, gitlink: false, ...overrides };
}

function fixture(t, options = {}) {
  const root = path.resolve('workspace/repo');
  const repo = { rootUri: uri(root) };
  const changes = new EventEmitter(), calls = [], opened = [], groups = [];
  const backend = { async validateRepositoryIdentity() { return { valid: options.valid !== false }; } };
  const tree = { output: { appendLine() {} }, backend, onDidChangeTreeData: changes.event };
  const vscode = { EventEmitter, Uri: { file: uri }, workspace: {},
    FileSystemError: { NoPermissions: () => Object.assign(new Error('NoPermissions'), { code: 'NoPermissions' }) },
    commands: { async executeCommand(...args) { calls.push(args); } } };
  const links = new LinkDocuments(vscode, tree, backend);
  t.after(() => links.dispose());
  const create = links.createVerified.bind(links);
  links.createVerified = async (fileUri, read, ...rest) => {
    opened.push(fileUri.fsPath);
    groups.push(rest[2]);
    return create(fileUri, read, ...rest);
  };
  const state = { alive: options.alive !== false, links, vscode, repo, calls, opened, groups,
    setAlive(value) { state.alive = value; } };
  const current = () => state.alive;
  state.open = (fileValue, contents, stashOid = oid) => openStashDiff({ vscode, links, repo, repoToken: 1, identity,
    oid: stashOid, index: 0, file: fileValue, contents, current });
  return state;
}

test('stash diffs read both sides from saved contents and never the working tree', async t => {
  const f = fixture(t);
  await f.open(file(), { old: text('saved old\n'), new: text('saved new\n') });
  assert.equal(f.calls.length, 1);
  const [command, left, right, title] = f.calls[0];
  assert.equal(command, 'vscode.diff');
  assert.equal(title, `${path.join(f.repo.rootUri.fsPath, 'src', 'file.txt')} (stash@{0})`);
  assert.match(left.toString(), /^gitpane-link:\/[0-9a-f-]{36}$/);
  assert.doesNotMatch(left.toString(), /workspace|repo|file\.txt/);
  assert.deepEqual(f.opened, [path.join(f.repo.rootUri.fsPath, 'src', 'file.txt'),
    path.join(f.repo.rootUri.fsPath, 'src', 'file.txt')]);
  assert.equal(await f.links.provideTextDocumentContent(left), 'saved old\n');
  assert.equal(await f.links.provideTextDocumentContent(right), 'saved new\n');
});

test('added, deleted and renamed stash files use an empty side and both saved paths', async t => {
  const added = fixture(t);
  await added.open(file({ status: 'A', old_path: null, new_path: 'added.txt', path: 'added.txt' }),
    { old: missing(), new: text('added\n') });
  assert.match(added.calls[0][1].toString(), /^gitpane-empty:/);
  assert.equal(await added.links.provideTextDocumentContent(added.calls[0][2]), 'added\n');

  const deleted = fixture(t);
  await deleted.open(file({ status: 'D', old_path: 'gone.txt', new_path: null, path: 'gone.txt' }),
    { old: text('gone\n'), new: missing() });
  assert.equal(await deleted.links.provideTextDocumentContent(deleted.calls[0][1]), 'gone\n');
  assert.match(deleted.calls[0][2].toString(), /^gitpane-empty:/);

  const renamed = fixture(t);
  const rename = file({ status: 'R', old_path: 'old name.txt', new_path: 'new name.txt', path: 'new name.txt' });
  assert.deepEqual(stashSides(rename), { left: 'old name.txt', right: 'new name.txt' });
  await renamed.open(rename, { old: text('old\n'), new: text('new\n') });
  assert.deepEqual(renamed.opened, [path.join(renamed.repo.rootUri.fsPath, 'old name.txt'),
    path.join(renamed.repo.rootUri.fsPath, 'new name.txt')]);
  assert.equal(renamed.calls[0][3], `${path.join(renamed.repo.rootUri.fsPath, 'new name.txt')} (stash@{0})`);
});

test('saved-untracked files always compare against the empty tree', async t => {
  const f = fixture(t);
  const saved = file({ source: 'saved-untracked', status: 'A', old_path: null, new_path: 'notes.txt', path: 'notes.txt' });
  assert.deepEqual(stashSides(saved), { left: null, right: 'notes.txt' });
  await f.open(saved, { old: missing(), new: text('saved untracked\n') });
  assert.match(f.calls[0][1].toString(), /^gitpane-empty:/);
  assert.equal(await f.links.provideTextDocumentContent(f.calls[0][2]), 'saved untracked\n');
});

test('saved submodule pointers render the stored gitlink text', async t => {
  const f = fixture(t);
  const pointer = file({ gitlink: true, path: 'child', old_path: 'child', new_path: 'child' });
  await f.open(pointer, { old: gitlink('1'.repeat(40)), new: gitlink('2'.repeat(40)) });
  assert.equal(await f.links.provideTextDocumentContent(f.calls[0][1]), `Subproject commit ${'1'.repeat(40)}\n`);
  assert.equal(await f.links.provideTextDocumentContent(f.calls[0][2]), `Subproject commit ${'2'.repeat(40)}\n`);
  assert.equal(stashFileLabel(pointer), 'M child (submodule)');
});

test('binary stash files and vanished selections never open a diff', async t => {
  const binaryFile = fixture(t);
  await assert.rejects(binaryFile.open(file({ binary: true }), { old: text(''), new: binary() }),
    /cannot show a text diff for the saved binary file src\/file\.txt/);
  assert.equal(binaryFile.calls.length, 0);

  const gone = fixture(t, { alive: false });
  await assert.rejects(gone.open(file(), { old: text('old'), new: text('new') }), /no longer available/);
  assert.equal(gone.calls.length, 0);

  const changed = fixture(t, { valid: false });
  await assert.rejects(changed.open(file(), { old: text('old'), new: text('new') }), /repository changed/);
  assert.equal(changed.calls.length, 0);
});

test('document cache keys separate equal repo, index and path with different stash OIDs', async t => {
  const f = fixture(t);
  const contents = { old: text('old'), new: text('new') };
  await f.open(file(), contents, oid);
  await f.open(file(), contents, oid);
  await f.open(file(), contents, 'c'.repeat(40));
  assert.equal(f.groups.length, 6);
  assert.deepEqual(f.groups.slice(0, 2), f.groups.slice(2, 4));   // same snapshot, same keys
  assert.notEqual(f.groups[0], f.groups[4]);
  assert.notEqual(f.groups[1], f.groups[5]);
  assert.ok(f.groups[0].includes(oid));
  assert.match(f.groups[4], /c{40}/);
  await assert.rejects(f.open(file(), contents, 'not-an-oid'), /verified stash contents/);
});

test('stash documents stay read-only and reject invalid payloads', async t => {
  const f = fixture(t);
  await f.open(file(), { old: text('old'), new: text('new') });
  assert.throws(() => f.links.writeFile(f.calls[0][1], new Uint8Array(), { create: true, overwrite: true }), /NoPermissions/);
  assert.throws(() => f.links.delete(f.calls[0][1], { recursive: false }), /NoPermissions/);

  assert.throws(() => validateStashList({ stash_oid: oid, repo_identity: identity, files: [
    file({ path: '../escape' })] }, oid, identity), /invalid stash file list/);
  assert.throws(() => validateStashList({ stash_oid: oid, repo_identity: identity, files: [
    file({ status: 'R', old_path: null })] }, oid, identity), /invalid stash file list/);
  assert.throws(() => validateStashList({ stash_oid: 'c'.repeat(40), repo_identity: identity, files: [file()] }, oid, identity),
    /invalid stash file list/);
  assert.throws(() => validateStashList({ stash_oid: oid, repo_identity: identity, files: [file(), file()] }, oid, identity),
    /duplicate stash files/);
  assert.throws(() => validateStashContent({ old: text('a'), new: { kind: 'binary', text: 'decoded' } }),
    /invalid stash file contents/);
});
