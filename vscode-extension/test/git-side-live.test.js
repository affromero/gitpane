'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { createLiveGitSide } = require('../src/git-side-documents');
const { MAX_REQUEST_BYTES } = require('../src/bridge-client');
const { GitMetadataBatches } = require('../src/git-metadata-batches');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { scheme, fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

function fixture(t) {
  const root = path.resolve('workspace/repo'), identity = 'a'.repeat(64);
  const vscode = { EventEmitter, FileType: { File: 1 }, FileChangeType: { Changed: 1 }, Uri: { file: uri }, workspace: {},
    FileSystemError: { FileNotFound: () => Object.assign(new Error('File not found'), { code: 'FileNotFound' }) } };
  const changes = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  vscode.workspace.onDidOpenTextDocument = opened.event;
  vscode.workspace.onDidCloseTextDocument = closed.event;
  const values = new Map(), calls = [], errors = [], logs = [];
  vscode.window = { showErrorMessage: message => errors.push(message) };
  let available = true, ancestor = true, onRead, revision = 0, volatile = false;
  const session = { repo: root, git: { key: 'git', executable: '/usr/bin/git' },
    snapshot: { roots: [root], rootSources: [root], rootIdentities: [identity] } };
  const backend = {
    async gitSideSession() { return session; },
    gitSideCurrent() { return true; },
    async gitSideOperation(_session, _identity, operation) {
      if (operation.kind === 'verified_content') {
        const key = `${operation.ref === ':0' ? '' : operation.ref}:${operation.path}`;
        const value = values.get(key);
        if (!value) throw Object.assign(new Error('Missing content'), { code: 'content_not_found' });
        const before = JSON.stringify(value.meta), bytes = Buffer.from(value.bytes);
        if (onRead) await onRead();
        if (before !== JSON.stringify(values.get(key)?.meta)) throw new Error('Git content changed while being read.');
        return bytes;
      }
      if (operation.items.length > 1) calls.push(operation.items);
      return { version: 1, volatile, replacements: 'a'.repeat(64), config: 'b'.repeat(64), results: operation.items.map(item => {
        const value = values.get(`${item.ref === ':0' ? '' : item.ref}:${item.path}`);
        return value ? { metadata: { ...value.meta } } : { error: { code: 'content_not_found', message: 'Missing content' } };
      }) };
    }
  };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event, output: { appendLine: value => logs.push(value) } }, backend);
  t.after(() => links.dispose());
  const repo = { rootUri: uri(root) };
  const put = (ref, relative, content, hash = '1', head = ref === 'HEAD' ? '2'.repeat(40) : null) => {
    values.set(`${ref}:${relative}`, { bytes: content, meta: { oid: hash.repeat(40), mode: 0o100644, size: 1, head_oid: head } });
  };
  const options = (ref, relative) => ({ vscode, links, backend, repo, repoIdentity: identity, repoToken: 7,
    side: { gitPath: relative, ref }, current: () => available, revision: () => revision,
    verifyAncestors: async () => ancestor, ancestorKey: 'chain' });
  return { links, backend, values, calls, errors, logs, opened, closed, put, options, setAvailable(value) { available = value; },
    setAncestor(value) { ancestor = value; }, setOnRead(value) { onRead = value; }, setVolatile(value) { volatile = value; }, signal() { revision++; } };
}

test('live Git sides update same-sized contents and recover after missing index entries', async t => {
  const f = fixture(t);
  f.put('', 'file.txt', 'one', '1');
  const file = await createLiveGitSide(f.options('', 'file.txt'));
  const events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  const initial = await f.links.stat(file);
  assert.equal((await f.links.readFile(file)).toString(), 'one');
  f.put('', 'file.txt', 'two', '3');
  await f.links.refresh();
  assert.equal(events.length, 1);
  assert.ok((await f.links.stat(file)).mtime > initial.mtime);
  assert.equal((await f.links.readFile(file)).toString(), 'two');
  f.values.delete(':file.txt');
  await f.links.refresh();
  await assert.rejects(f.links.readFile(file), error => error.code === 'FileNotFound');
  f.put('', 'file.txt', 'new', '4');
  await f.links.refresh();
  assert.equal(events.length, 3);
  assert.equal((await f.links.readFile(file)).toString(), 'new');
});

test('live Git sides validate refs, paths and exact metadata', async t => {
  const f = fixture(t);
  for (const ref of ['', ':1', ':2', ':3', 'HEAD', 'b'.repeat(40)]) {
    const head = ref === 'HEAD' ? '2'.repeat(40) : /^[a-f0-9]{40}$/.test(ref) ? ref : null;
    f.put(ref, 'blocked/:(glob)*.bin', '\0binary', '1', head);
    const file = await createLiveGitSide(f.options(ref, 'blocked/:(glob)*.bin'));
    assert.equal((await f.links.readFile(file)).toString(), '\0binary');
  }
  for (const side of [{ gitPath: '../outside', ref: 'HEAD' }, { gitPath: 'file', ref: 'HEAD~1' }]) {
    await assert.rejects(createLiveGitSide({ ...f.options('', 'file'), side }), /cannot open/i);
  }
  f.put('', 'file', 'content');
  f.values.get(':file').meta.mode = 0o160000;
  await assert.rejects(createLiveGitSide(f.options('', 'file')), /invalid Git file metadata/i);
});

test('live Git sides reject changed data during a read and stale repository ancestry', async t => {
  const f = fixture(t);
  f.put('HEAD', 'file.txt', 'old', '1');
  const file = await createLiveGitSide(f.options('HEAD', 'file.txt'));
  let started, release;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnRead(async () => { started(); await new Promise(resolve => { release = resolve; }); });
  const reading = f.links.readFile(file);
  await began;
  f.put('HEAD', 'file.txt', 'new', '3', '4'.repeat(40));
  release();
  await assert.rejects(reading, /changed|Missing content/i);
  f.setOnRead(undefined);
  f.setAncestor(false);
  await assert.rejects(f.links.readFile(file), /no longer/i);
  f.setAncestor(true);
  f.setAvailable(false);
  await assert.rejects(f.links.readFile(file), /no longer/i);
});

test('SCM changes invalidate converted output after an in-flight read finishes', async t => {
  const f = fixture(t);
  f.setVolatile(true);
  f.put('', 'file.txt', 'first', '1');
  const file = await createLiveGitSide(f.options('', 'file.txt'));
  const before = await f.links.stat(file), events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  f.put('', 'file.txt', 'other', '1');
  f.signal();
  await f.links.refresh();
  assert.equal(events.length, 1);
  assert.ok((await f.links.stat(file)).mtime > before.mtime);
  assert.equal((await f.links.readFile(file)).toString(), 'other');
  let started, release;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnRead(async () => { started(); await new Promise(resolve => { release = resolve; }); });
  const reading = f.links.readFile(file);
  await began;
  f.signal();
  await f.links.refresh();
  f.signal();
  await f.links.refresh();
  assert.equal(events.length, 1);
  release();
  assert.equal((await reading).toString(), 'other');
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(events.length, 2);
});

test('ancestor changes during a binary read reject stale contents', async t => {
  const f = fixture(t);
  f.put('', 'file.txt', 'content');
  const file = await createLiveGitSide(f.options('', 'file.txt'));
  let started, release;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnRead(async () => { started(); await new Promise(resolve => { release = resolve; }); });
  const reading = f.links.readFile(file);
  await began;
  f.setAncestor(false);
  release();
  await assert.rejects(reading, /no longer/i);
});

test('a hundred open Git sides share one bounded refresh batch', async t => {
  const f = fixture(t);
  for (let index = 0; index < 100; index++) {
    const name = `file-${index}.txt`;
    f.put('', name, `content-${index}`);
    await createLiveGitSide(f.options('', name));
  }
  await f.links.refresh();
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].length, 100);
  const events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  f.signal();
  await f.links.refresh();
  assert.equal(events.length, 0);
  assert.equal(f.calls.length, 2);
});

test('concurrent open sides share metadata while each keeps its own contents', async t => {
  const f = fixture(t);
  const raw = f.backend.gitSideOperation.bind(f.backend);
  const batches = new GitMetadataBatches((owner, identity, items, signal) => raw(owner, identity, { kind: 'metadata', items }, signal));
  t.after(() => batches.dispose());
  f.backend.gitSideOperation = (owner, identity, operation, signal) => operation.kind === 'metadata' && operation.items.length === 1 ?
    batches.request(owner, identity, operation.items[0], signal) : raw(owner, identity, operation, signal);
  const paths = Array.from({ length: 100 }, (_, index) => `file-${index}.txt`);
  for (const name of paths) f.put('', name, name);
  const files = await Promise.all(paths.map(name => createLiveGitSide(f.options('', name))));
  assert.equal(f.calls.length, 1);
  await Promise.all(files.map(file => f.links.stat(file)));
  assert.equal(f.calls.length, 2);
  assert.equal((await f.links.readFile(files[0])).toString(), paths[0]);
  assert.equal((await f.links.readFile(files.at(-1))).toString(), paths.at(-1));
});

test('shared metadata does not authorize a side whose ancestry changed', async t => {
  const f = fixture(t);
  f.put('', 'file.txt', 'content');
  const raw = f.backend.gitSideOperation.bind(f.backend);
  let started, release, valid = true;
  const waiting = new Promise(resolve => { started = resolve; });
  const batches = new GitMetadataBatches(async (owner, identity, items, signal) => {
    started();
    await new Promise(resolve => { release = resolve; });
    return raw(owner, identity, { kind: 'metadata', items }, signal);
  });
  t.after(() => batches.dispose());
  f.backend.gitSideOperation = (owner, identity, operation, signal) => operation.kind === 'metadata' && operation.items.length === 1 ?
    batches.request(owner, identity, operation.items[0], signal) : raw(owner, identity, operation, signal);
  const first = createLiveGitSide({ ...f.options('', 'file.txt'), contextKey: 'first', verifyAncestors: async () => valid });
  const second = createLiveGitSide({ ...f.options('', 'file.txt'), contextKey: 'second', verifyAncestors: async () => true });
  const results = Promise.allSettled([first, second]);
  await waiting;
  valid = false;
  release();
  const [stale, current] = await results;
  assert.equal(stale.status, 'rejected');
  assert.match(stale.reason.message, /no longer/i);
  assert.equal(current.status, 'fulfilled');
  assert.equal((await f.links.readFile(current.value)).toString(), 'content');
});

test('converted diffs coalesce changes until all overlapping reads complete', async t => {
  const f = fixture(t);
  f.setVolatile(true);
  f.put('', 'file.txt', 'converted');
  const file = await createLiveGitSide(f.options('', 'file.txt'));
  const releases = [], started = [];
  f.setOnRead(() => new Promise(resolve => { releases.push(resolve); started.shift()?.(); }));
  const waitForRead = () => new Promise(resolve => started.push(resolve));
  const firstStarted = waitForRead();
  const first = f.links.readFile(file);
  await firstStarted;
  const secondStarted = waitForRead();
  const second = f.links.readFile(file);
  await secondStarted;
  const events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  f.signal();
  await f.links.refresh();
  releases[0]();
  await first;
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(events.length, 0);
  releases[1]();
  await second;
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(events.length, 1);
});

test('live metadata batches respect the bridge wire limit', async t => {
  const f = fixture(t);
  for (let index = 0; index < 30; index++) {
    const name = `${index}-${'x'.repeat(50000)}`;
    f.put('', name, 'content');
    await createLiveGitSide(f.options('', name));
  }
  await f.links.refresh();
  assert.ok(f.calls.length > 1);
  for (const items of f.calls) {
    const payload = { version: 1, workspace_roots: [path.resolve('workspace/repo')],
      workspace_root_identities: ['a'.repeat(64)], workspace_root_sources: [path.resolve('workspace/repo')],
      repo: path.resolve('workspace/repo'), expected_repo_identity: 'a'.repeat(64),
      git_executable: '/usr/bin/git', operation: { kind: 'metadata', items } };
    assert.ok(Buffer.byteLength(`${JSON.stringify(payload)}\n`) <= MAX_REQUEST_BYTES);
  }
});

test('separate worktree contexts do not share ancestor validity', async t => {
  const f = fixture(t);
  f.put('', 'file.txt', 'content');
  let firstValid = true;
  const stale = await createLiveGitSide({ ...f.options('', 'file.txt'), contextKey: 'worktree',
    verifyAncestors: async () => firstValid });
  const valid = await createLiveGitSide({ ...f.options('', 'file.txt'), contextKey: 'direct',
    verifyAncestors: async () => true });
  const events = [];
  f.links.onDidChangeFile(value => events.push(...value));
  firstValid = false;
  await f.links.refresh();
  assert.deepEqual(events.map(event => event.uri.toString()), [stale.toString()]);
  assert.equal((await f.links.readFile(valid)).toString(), 'content');
});

test('batch failures and malformed missing responses are reported once for the repository', async t => {
  for (const failure of ['unsupported_backend', 'repository_error', 'malformed']) {
    const f = fixture(t);
    f.put('', 'file.txt', 'content');
    await createLiveGitSide(f.options('', 'file.txt'));
    f.backend.gitSideOperation = async () => failure === 'malformed'
      ? { version: 1, volatile: false, replacements: 'a'.repeat(64), config: 'b'.repeat(64), results: [{ error: { code: 'content_not_found' }, metadata: {} }] }
      : Promise.reject(Object.assign(new Error(`failure ${failure}`), { code: failure }));
    await f.links.refresh();
    assert.equal(f.errors.length, 1);
    assert.equal(f.logs.length, 1);
    assert.match(f.errors[0], /could not refresh a diff/i);
    await f.links.refresh();
    assert.equal(f.errors.length, 1, 'the same unresolved failure is not repeatedly shown');
  }
});

test('an unfinished diff releases its allocated side at the document limit', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = fixture(t);
  f.put('', 'file.txt', 'content');
  const opened = await createLiveGitSide(f.options('', 'file.txt'));
  f.opened.fire({ uri: opened });
  for (let index = 0; index < 254; index++) await createLiveGitSide(f.options('', 'file.txt'));
  const unfinished = await createLiveGitSide(f.options('', 'file.txt'));
  await assert.rejects(createLiveGitSide(f.options('', 'file.txt')), /too many/i);
  t.mock.timers.tick(60000);
  await assert.rejects(f.links.readFile(unfinished), /no longer/i);
  assert.equal((await f.links.readFile(opened)).toString(), 'content');
  assert.ok(await createLiveGitSide(f.options('', 'file.txt')));
});
