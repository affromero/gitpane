'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { createGitSide } = require('../src/git-side-documents');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { scheme, fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

function fixture(t, ref = 'HEAD') {
  const root = path.resolve('workspace/repo');
  const changes = new EventEmitter();
  const vscode = { EventEmitter, FileType: { File: 1 }, FileChangeType: { Changed: 1 },
    Uri: { file: uri }, workspace: {} };
  let metadata = { oid: 'a'.repeat(40), mode: 0o100644, size: 4,
    head_oid: ref === 'HEAD' ? 'b'.repeat(40) : null };
  let recordCurrent = true, repoCurrent = true, ancestorsCurrent = true;
  let metadataCalls = 0, verifyCalls = 0;
  const backend = { async repoBlobMetadata() { metadataCalls++; return { ...metadata }; } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => links.dispose());
  const reads = [];
  const repo = { rootUri: uri(root), async buffer(gitRef, relative) {
    reads.push([gitRef, relative]); return Buffer.from([0, 255, 13, 10]);
  } };
  const options = { vscode, links, backend, repo, repoIdentity: 'c'.repeat(64), repoToken: 1,
    side: { gitPath: 'blocked/:(glob)*.bin', ref }, record: { path: 'blocked/:(glob)*.bin', group: 'working' },
    current: () => repoCurrent, verify: async () => { verifyCalls++; return recordCurrent; },
    verifyAncestors: async () => ancestorsCurrent, ancestorKey: 'chain' };
  return { options, links, reads, setMetadata(value) { metadata = value; }, getMetadata() { return { ...metadata }; },
    setRecord(value) { recordCurrent = value; }, setRepo(value) { repoCurrent = value; }, setAncestors(value) { ancestorsCurrent = value; },
    resetCalls() { metadataCalls = 0; verifyCalls = 0; }, calls() { return { metadataCalls, verifyCalls }; } };
}

test('blocked historical binary data is read from the selected repository and captured commit', async t => {
  const f = fixture(t);
  const file = await createGitSide(f.options);
  assert.equal(file.scheme, 'gitpane-blob');
  assert.deepEqual(await f.links.readFile(file), Buffer.from([0, 255, 13, 10]));
  assert.deepEqual(f.reads, [['b'.repeat(40), 'blocked/:(glob)*.bin']]);
});

test('index and conflict sides reject changed metadata or stale repository ancestry', async t => {
  for (const ref of ['', ':1', ':2', ':3']) {
    const f = fixture(t, ref);
    const file = await createGitSide(f.options);
    assert.deepEqual(await f.links.readFile(file), Buffer.from([0, 255, 13, 10]));
    assert.deepEqual(f.reads, [[ref, 'blocked/:(glob)*.bin']]);
    f.setMetadata({ ...f.getMetadata(), oid: 'd'.repeat(40) });
    await assert.rejects(f.links.readFile(file), /no longer|changed/i);
    f.setMetadata({ ...f.getMetadata(), oid: 'a'.repeat(40) });
    f.setAncestors(false);
    await assert.rejects(f.links.readFile(file), /no longer|changed/i);
    f.setAncestors(true);
    f.setRecord(false);
    await assert.rejects(f.links.readFile(file), /no longer|changed/i);
    f.setRecord(true);
    f.setRepo(false);
    await assert.rejects(f.links.readFile(file), /no longer/i);
  }
});

test('Git-side documents reject invalid paths, refs and metadata', async t => {
  const f = fixture(t);
  for (const side of [{ gitPath: '../outside', ref: 'HEAD' }, { gitPath: 'file', ref: 'unexpected' }]) {
    await assert.rejects(createGitSide({ ...f.options, side }), /cannot open/i);
  }
  f.setMetadata({ ...f.getMetadata(), mode: 0o160000 });
  await assert.rejects(createGitSide(f.options), /invalid Git file metadata/i);
});

test('a binary read verifies the change and metadata before and after the buffer', async t => {
  const f = fixture(t, ':2');
  const file = await createGitSide(f.options);
  f.resetCalls();
  assert.deepEqual(await f.links.readFile(file), Buffer.from([0, 255, 13, 10]));
  assert.deepEqual(f.calls(), { metadataCalls: 2, verifyCalls: 2 });
});

test('in-flight binary reads reject a changed record, blob or ancestor', async t => {
  for (const changed of ['record', 'blob', 'ancestor']) {
    const f = fixture(t, ':2');
    let started, release;
    const pendingRead = new Promise(resolve => { started = resolve; });
    f.options.repo.buffer = async () => { started(); await new Promise(resolve => { release = resolve; }); return Buffer.from('data'); };
    const file = await createGitSide(f.options);
    const reading = f.links.readFile(file);
    await pendingRead;
    if (changed === 'record') f.setRecord(false);
    if (changed === 'blob') f.setMetadata({ ...f.getMetadata(), oid: 'd'.repeat(40) });
    if (changed === 'ancestor') f.setAncestors(false);
    release();
    await assert.rejects(reading, /changed|no longer/i);
  }
});

test('record, blob and ancestor changes invalidate already open binary documents', async t => {
  for (const changed of ['record', 'blob', 'ancestor']) {
    const f = fixture(t, ':2');
    const file = await createGitSide(f.options);
    const events = [];
    f.links.onDidChangeFile(value => events.push(...value));
    if (changed === 'record') f.setRecord(false);
    if (changed === 'blob') f.setMetadata({ ...f.getMetadata(), oid: 'd'.repeat(40) });
    if (changed === 'ancestor') f.setAncestors(false);
    await f.links.refresh();
    assert.equal(events.length, 1);
    assert.equal(events[0].uri.toString(), file.toString());
    await assert.rejects(f.links.readFile(file), /changed|no longer/i);
  }
});

test('a stale worktree guard cannot invalidate the same direct repository diff', async t => {
  const f = fixture(t, ':2');
  let worktreeCurrent = true;
  const worktree = await createGitSide({ ...f.options, contextKey: 'worktree', current: () => worktreeCurrent });
  const direct = await createGitSide({ ...f.options, contextKey: 'direct' });
  const events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  worktreeCurrent = false;
  await f.links.refresh();
  assert.deepEqual(events.map(event => event.uri.toString()), [worktree.toString()]);
  assert.deepEqual(await f.links.readFile(direct), Buffer.from([0, 255, 13, 10]));
});
