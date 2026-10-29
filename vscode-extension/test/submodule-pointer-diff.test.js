'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { openPointerDiff, isPointerComparison, selectedSides } = require('../src/submodule-pointer-diff');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}
function uri(value, scheme = 'file') {
  return { fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}
function fixture(t, record, sides, head = 'a'.repeat(40)) {
  const root = path.resolve('workspace/repo'), repo = { rootUri: uri(root) };
  let alive = true, selected = true, revalidate = true, reads = 0, compareHook = () => {}, contentHook = () => {};
  const changes = new EventEmitter(), commands = [];
  const tree = { git: { get repositories() { return [{ ...repo }]; } }, backendStatuses: new Map([[repo.rootUri.toString(), { repo_identity: 'b'.repeat(64) }]]),
    isCurrentRepository: (_repo, token) => alive && token === 1, onDidChangeTreeData: changes.event };
  const node = { repo, repoToken: 1, record, group: record.group, fileId: 'file', change: { uri: uri(path.join(root, record.path)) } };
  const snapshot = { repo_identity: 'b'.repeat(64), head_oid: head, record, sides };
  const backend = { async submodulePointerCompare() { await compareHook(); return structuredClone(snapshot); },
    async submodulePointerContent(_repo, _record, _identity, _head, _sides, side) {
      await contentHook(); reads++;
      const entry = snapshot.sides[side];
      return { content: entry.kind === 'gitlink' ? `Subproject commit ${entry.oid}\n` : `contents of ${entry.path}\n` };
    } };
  const vscode = { EventEmitter, Uri: { file: uri }, workspace: {},
    commands: { async executeCommand(...args) { commands.push(args); } } };
  const links = new LinkDocuments(vscode, tree, backend);
  t.after(() => links.dispose());
  const provider = { currentChange: () => selected ? node : undefined, async revalidateChange() { return revalidate; } };
  return { node, tree, repo, backend, links, vscode, provider, commands, snapshot, get reads() { return reads; },
    setAlive(value) { alive = value; }, setSelected(value) { selected = value; }, setRevalidate(value) { revalidate = value; },
    onCompare(value) { compareHook = value; }, onContent(value) { contentHook = value; } };
}

const gitlink = (name, value) => ({ path: name, kind: 'gitlink', mode: 57344, oid: value.repeat(40) });
const blob = name => ({ path: name, kind: 'blob', mode: 33188, oid: 'c'.repeat(40) });
const record = (kind, group = 'index') => ({ group, kind, path: 'child',
  original_path: kind === 'index_renamed' ? 'old-child' : null,
  conflict_stages: group === 'merge' ? { base: 'child', ours: 'child', theirs: 'child' } : null, is_submodule: true });

test('staged rename and type changes open guarded pointer and blob contents', async t => {
  for (const [change, sides] of [
    [record('index_renamed'), { base: gitlink('old-child', '1'), index: gitlink('child', '2') }],
    [record('index_type_changed'), { base: gitlink('child', '1'), index: blob('child') }]
  ]) {
    const f = fixture(t, change, sides);
    assert.equal(isPointerComparison(change), true);
    await openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode);
    assert.equal(f.commands.length, 1);
    assert.equal(f.commands[0][0], 'vscode.diff');
    assert.equal(await f.links.provideTextDocumentContent(f.commands[0][1]), `Subproject commit ${'1'.repeat(40)}\n`);
    const right = await f.links.provideTextDocumentContent(f.commands[0][2]);
    assert.equal(right, sides.index.kind === 'gitlink' ? `Subproject commit ${'2'.repeat(40)}\n` : 'contents of child\n');
    assert.deepEqual(f.tree.git.repositories, [f.repo], 'pointer comparisons do not open a child repository');
  }
});

test('conflict pointer comparisons select available stages and empty sides', async t => {
  const change = record('conflicted', 'merge');
  const sides = { base: gitlink('child', '1'), ours: gitlink('child', '2'), theirs: gitlink('child', '3') };
  assert.deepEqual(selectedSides(change, sides), ['ours', 'theirs']);
  assert.deepEqual(selectedSides(change, { base: sides.base, ours: null, theirs: null }), ['base', null]);
  const f = fixture(t, change, sides);
  await openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode);
  assert.equal(await f.links.provideTextDocumentContent(f.commands[0][1]), `Subproject commit ${'2'.repeat(40)}\n`);
  assert.equal(await f.links.provideTextDocumentContent(f.commands[0][2]), `Subproject commit ${'3'.repeat(40)}\n`);
});

test('a deleted submodule uses an empty right side', async t => {
  const change = record('index_deleted');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: null });
  await openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode);
  assert.equal(await f.links.provideTextDocumentContent(f.commands[0][1]), `Subproject commit ${'1'.repeat(40)}\n`);
  assert.equal(f.commands[0][2].toString().startsWith('gitpane-empty:'), true);
});

test('same-record pointer changes and stale repository or selection block the diff and document reads', async t => {
  const change = record('index_renamed'), sides = { base: gitlink('old-child', '1'), index: gitlink('child', '2') };
  const f = fixture(t, change, sides);
  await openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode);
  const document = f.commands[0][1];
  f.snapshot.sides.index.oid = '4'.repeat(40);
  await assert.rejects(f.links.provideTextDocumentContent(document), /no longer/i);
  f.setAlive(false);
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode), /no longer/i);
  f.setAlive(true); f.setSelected(false);
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode), /no longer/i);
  assert.equal(f.commands.length, 1);
});

test('directory sides and working submodule types are not silently represented as pointers', async t => {
  const change = record('index_type_changed');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: { path: 'child', kind: 'tree', mode: 16384, oid: 'c'.repeat(40) } });
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode), /director|unsupported/i);
  assert.equal(isPointerComparison({ ...change, group: 'working', kind: 'type_changed' }), false);
  assert.equal(f.commands.length, 0);
});

test('unreadable pointer sides report the backend error without opening a diff', async t => {
  const change = record('index_type_changed');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: blob('child') });
  f.onContent(() => { throw new Error('Binary pointer sides cannot be opened as text.'); });
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode), /binary/i);
  assert.equal(f.commands.length, 0);
});

test('changed endpoints during selection do not dispatch a diff', async t => {
  const change = record('index_renamed');
  const f = fixture(t, change, { base: gitlink('old-child', '1'), index: gitlink('child', '2') });
  let comparisons = 0;
  f.onCompare(() => { if (++comparisons === 2) f.snapshot.sides.index.oid = '4'.repeat(40); });
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode), /no longer/i);
  assert.equal(f.commands.length, 0);
});

test('nested pointer documents use the inspected identity and reject a stale ancestor', async t => {
  const change = record('index_deleted');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: null });
  f.tree.backendStatuses.clear();
  let ancestor = true;
  const options = { repoIdentity: 'b'.repeat(64), current: () => ancestor,
    verifyAncestors: async () => ancestor, ancestorKey: 'parent-chain' };
  await openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode, options);
  const document = f.commands[0][1];
  assert.equal(await f.links.provideTextDocumentContent(document), `Subproject commit ${'1'.repeat(40)}\n`);
  ancestor = false;
  await assert.rejects(f.links.provideTextDocumentContent(document), /no longer/i);
  assert.equal(f.commands.length, 1);
});

test('nested pointer selection lost during parent or endpoint validation cannot open a diff', async t => {
  const change = record('index_deleted');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: null });
  let ancestor = true;
  f.onCompare(() => { f.setSelected(false); });
  await assert.rejects(openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode,
    { repoIdentity: 'b'.repeat(64), current: () => ancestor, verifyAncestors: async () => ancestor, ancestorKey: 'parent-chain' }), /no longer/i);
  assert.equal(f.commands.length, 0);
});

test('an ancestor changing during a held leaf comparison cannot authorize a pointer document', async t => {
  const change = record('index_deleted');
  const f = fixture(t, change, { base: gitlink('child', '1'), index: null });
  let ancestor = true, comparisons = 0, release, entered, accepted;
  const started = new Promise(resolve => { entered = resolve; });
  f.onCompare(async () => {
    if (++comparisons === 2) { entered(); await new Promise(resolve => { release = resolve; }); }
  });
  f.links.createVerified = async (_uri, _read, guard) => {
    accepted = await guard();
    if (!accepted) throw new Error('stale ancestor');
    return uri('/virtual');
  };
  const pending = openPointerDiff(f.node, f.provider, f.tree, f.backend, f.links, f.vscode,
    { repoIdentity: 'b'.repeat(64), current: () => true, verifyAncestors: async () => ancestor, ancestorKey: 'parent-chain' });
  await started;
  ancestor = false;
  release();
  await assert.rejects(pending, /stale ancestor/i);
  assert.equal(accepted, false);
  assert.equal(f.commands.length, 0);
});
