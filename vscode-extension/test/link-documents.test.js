'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { LinkDocuments, statusRevision } = require('../src/link-documents');
const { MAX_REQUEST_BYTES } = require('../src/bridge-client');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

async function settleRefresh(docs) {
  const deadline = Date.now() + 5000;
  do {
    await new Promise(resolve => setImmediate(resolve));
    assert.ok(Date.now() < deadline, 'symlink refresh did not finish');
  } while (docs.refreshing);
}

test('commit pointer documents are guarded, read-only, and do not watch filesystem paths', async t => {
  const changes = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidCloseTextDocument: closed.event } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, {});
  t.after(() => docs.dispose());
  let current = true;
  const text = `Subproject commit ${'a'.repeat(40)}\n`;
  const document = await docs.createStatic(uri('/repo/child'), text, () => current);
  assert.equal(await docs.provideTextDocumentContent(document), text);
  assert.deepEqual(docs.watchedPaths('/repo'), []);
  await assert.rejects(docs.createStatic(uri('/repo/child'), 'unexpected content', () => true), /invalid/i);
  current = false;
  await assert.rejects(docs.provideTextDocumentContent(document), /no longer/i);
  closed.fire({ uri: document });
  await assert.rejects(docs.provideTextDocumentContent(document), /no longer available/i);
});

test('many open pointer documents share lightweight identity checks and invalidate after replacement', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const changes = new EventEmitter();
  const vscode = { EventEmitter, workspace: {} };
  let calls = 0, valid = true;
  const backend = { async validateRepositoryIdentity() { calls++; return { valid }; } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  const guard = () => docs.validateStaticIdentity('/repo', '1'.repeat(64), () => true);
  const pointers = [];
  for (let index = 0; index < 20; index++) {
    pointers.push(await docs.createStatic(uri('/repo/child'), `Subproject commit ${'a'.repeat(40)}\n`, guard, () => true, 'same-repo'));
  }
  const notified = [];
  docs.onDidChange(value => notified.push(value.toString()));
  calls = 0;
  valid = false;
  changes.fire();
  t.mock.timers.tick(50);
  await settleRefresh(docs);
  assert.equal(calls, 1, 'one lightweight check validates all pointer documents for this repository');
  assert.equal(notified.length, pointers.length);
  await assert.rejects(docs.provideTextDocumentContent(pointers[0]), /no longer/i);
});

test('many mutable pointer documents batch refreshes, invalidate stale rows, and surface backend errors', async t => {
  const changes = new EventEmitter(), errors = [];
  const vscode = { EventEmitter, workspace: {}, window: { showErrorMessage: message => errors.push(message) } };
  const states = new Map(), batches = [];
  let offline = false, guards = 0;
  const backend = { async validatePointerBatch(repo, identity, snapshots) {
    batches.push({ repo, identity, snapshots });
    if (offline) throw new Error('pointer worker unavailable');
    return { results: snapshots.map(item => states.get(item.record.path) ? { valid: true } :
      { valid: false, code: 'stale_target', message: 'Pointer changed' }) };
  } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  const documents = [];
  for (let index = 0; index < 128; index++) {
    const file = `child-${index}`;
    states.set(file, true);
    const snapshot = { record: { path: file }, head_oid: null, sides: {} };
    const batch = { repo: '/repo', identity: '1'.repeat(64), snapshot };
    const guard = async () => { guards++; return states.get(file); };
    for (let side = 0; side < 2; side++) {
      documents.push(await docs.createVerified(uri(`/repo/${file}`), async () => file, guard, () => true, file, batch));
    }
  }
  const notified = [];
  docs.onDidChange(value => notified.push(value.toString()));
  guards = 0;
  states.set('child-3', false);
  await docs.refresh();
  assert.equal(batches.length, 1);
  assert.equal(batches[0].snapshots.length, 128);
  assert.equal(guards, 0, 'background validation never scans every document separately');
  assert.equal(notified.length, 2, 'both sides of the changed pointer are invalidated');
  await assert.rejects(docs.provideTextDocumentContent(documents[6]), /no longer/i);
  offline = true;
  await docs.refresh();
  assert.equal(batches.length, 2);
  assert.equal(errors.length, 1, 'a worker failure is reported once per repository');
  assert.match(errors[0], /worker unavailable/i);
});

test('pointer batches fit the wire limit and reject an oversized item independently', async t => {
  const changes = new EventEmitter(), errors = [], calls = [];
  const vscode = { EventEmitter, workspace: {}, window: { showErrorMessage: value => errors.push(value) } };
  const backend = { async validatePointerBatch(repo, identity, snapshots) {
    const wire = `${JSON.stringify({ id: Number.MAX_SAFE_INTEGER, method: 'submodule_pointer_validate_batch',
      params: { repo, expected_repo_identity: identity, snapshots } })}\n`;
    assert.ok(Buffer.byteLength(wire) <= MAX_REQUEST_BYTES, 'every batch must fit the bridge request limit');
    calls.push(snapshots);
    return { results: snapshots.map(() => ({ valid: true })) };
  } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  const sidePath = 'å'.repeat(1900);
  const documents = [];
  for (let index = 0; index < 110; index++) {
    const snapshot = { record: { path: `${sidePath}${index}` }, head_oid: null,
      sides: { base: { path: sidePath }, ours: { path: sidePath }, theirs: { path: sidePath } } };
    documents.push(await docs.createVerified(uri(`/repo/${index}`), async () => 'valid', async () => true, () => true,
      String(index), { repo: '/repo', identity: 'a'.repeat(64), snapshot }));
  }
  const oversized = { record: { path: 'x'.repeat(MAX_REQUEST_BYTES) }, head_oid: null, sides: {} };
  const oversizedDoc = await docs.createVerified(uri('/repo/oversized'), async () => 'oversized', async () => true, () => true,
    'oversized', { repo: '/repo', identity: 'a'.repeat(64), snapshot: oversized });
  const notified = [];
  docs.onDidChange(value => notified.push(value.toString()));
  await docs.refresh();
  assert.ok(calls.length > 1, 'long UTF-8 paths must produce multiple requests');
  assert.equal(calls.flat().length, 110, 'only the individually oversized snapshot is rejected');
  assert.deepEqual(notified, [oversizedDoc.toString()]);
  assert.match(errors[0], /snapshot exceeds.*request size/i);
  assert.equal(await docs.provideTextDocumentContent(documents[0]), 'valid');
});

test('a pointer document created during a held batch is not marked stale', async t => {
  const changes = new EventEmitter();
  const vscode = { EventEmitter, workspace: {} };
  let release, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const backend = { async validatePointerBatch(_repo, _identity, snapshots) {
    entered();
    await new Promise(resolve => { release = resolve; });
    return { results: snapshots.map(() => ({ valid: false, code: 'stale_target', message: 'changed' })) };
  } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  const batch = name => ({ repo: '/repo', identity: 'a'.repeat(64), snapshot: { record: { path: name }, head_oid: null, sides: {} } });
  const old = await docs.createVerified(uri('/repo/old'), async () => 'old', async () => true, () => true, 'old', batch('old'));
  const notifications = [];
  docs.onDidChange(value => notifications.push(value.toString()));
  const refreshing = docs.refresh();
  await started;
  const added = await docs.createVerified(uri('/repo/new'), async () => 'new', async () => true, () => true, 'new', batch('new'));
  release();
  await refreshing;
  assert.deepEqual(notifications, [old.toString()]);
  assert.equal(await docs.provideTextDocumentContent(added), 'new');
});

test('nested pointer refresh rechecks shared ancestors after the leaf batch', async t => {
  const changes = new EventEmitter();
  const vscode = { EventEmitter, workspace: {} };
  let validAncestor = true, checks = 0, release, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const backend = { async validatePointerBatch(_repo, _identity, snapshots) {
    entered(); await new Promise(resolve => { release = resolve; });
    return { results: snapshots.map(() => ({ valid: true })) };
  } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  const notifications = [];
  docs.onDidChange(value => notifications.push(value.toString()));
  for (const name of ['one', 'two']) {
    const batch = { repo: '/repo/child', identity: 'a'.repeat(64), ancestorKey: 'same-chain',
      verifyAncestors: async () => { checks++; return validAncestor; },
      snapshot: { record: { path: name }, head_oid: null, sides: {} } };
    await docs.createVerified(uri(`/repo/child/${name}`), async () => name, async () => validAncestor, () => true, name, batch);
  }
  const pending = docs.refresh();
  await started;
  validAncestor = false;
  release();
  await pending;
  assert.equal(checks, 2, 'one shared ancestor check runs before and after the leaf batch');
  assert.equal(notifications.length, 2, 'both nested pointer documents are invalidated');
});

test('nested pointer checks do not share ancestry across worktree contexts', async t => {
  const changes = new EventEmitter();
  const vscode = { EventEmitter, workspace: {} };
  const backend = { async validatePointerBatch(_repo, _identity, snapshots) {
    return { results: snapshots.map(() => ({ valid: true })) };
  } };
  const docs = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => docs.dispose());
  let worktree = true;
  const make = async (contextKey, verifyAncestors) => {
    const batch = { repo: '/repo/child', identity: 'a'.repeat(64), ancestorKey: 'same-chain', contextKey,
      verifyAncestors, snapshot: { record: { path: 'file' }, head_oid: null, sides: {} } };
    return docs.createVerified(uri('/repo/child/file'), async () => 'value', async () => true,
      () => true, contextKey, batch);
  };
  const stale = await make('worktree', async () => worktree);
  const direct = await make('direct', async () => true);
  const notifications = [];
  docs.onDidChange(value => notifications.push(value.toString()));
  worktree = false;
  await docs.refresh();
  assert.deepEqual(notifications, [stale.toString()]);
  assert.equal(await docs.provideTextDocumentContent(direct), 'value');
});

test('symlink documents show link text, refresh, and expire when closed', { skip: process.platform === 'win32' }, async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-link-doc-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const repo = path.join(root, 'repo');
  fs.mkdirSync(repo);
  const outside = path.join(root, 'outside');
  fs.writeFileSync(outside, 'outside contents');
  const file = path.join(repo, 'link');
  fs.symlinkSync(outside, file);
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  let reads = 0;
  const backend = { async readWorkingSymlink(base, relative) { reads++; return { target: await fs.promises.readlink(path.join(base, relative)) }; } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event }, backend);
  t.after(() => links.dispose());
  let current = true;
  const document = await links.create(uri(file), repo, 'link', () => current);
  const duplicate = await links.create(uri(file), repo, 'link', () => current);
  assert.deepEqual(links.watchedPaths(repo), ['link']);
  assert.deepEqual(links.watchedPaths(root), []);
  opened.fire({ uri: document });
  assert.equal(await links.provideTextDocumentContent(document), outside);
  const notifications = [];
  links.onDidChange(value => notifications.push(value.toString()));
  fs.unlinkSync(file);
  fs.writeFileSync(path.join(repo, 'inside'), 'inside contents');
  fs.symlinkSync('inside', file);
  assert.equal(await links.provideTextDocumentContent(document), 'inside');
  fs.unlinkSync(file);
  fs.symlinkSync('missing-target', file);
  const beforeRefresh = reads;
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, [document.toString(), duplicate.toString()]);
  assert.equal(reads - beforeRefresh, 1, 'duplicate documents share the scoped link check');
  assert.equal(await links.provideTextDocumentContent(document), 'missing-target');
  current = false;
  await assert.rejects(links.provideTextDocumentContent(document), /no longer in the workspace/i);
  closed.fire({ uri: document });
  assert.deepEqual(links.watchedPaths(repo), ['link']);
  closed.fire({ uri: duplicate });
  assert.deepEqual(links.watchedPaths(repo), []);
  await assert.rejects(links.provideTextDocumentContent(document), /no longer available/i);
});

test('an older poll cannot erase a newly watched link status', () => {
  let status = { head_oid: 'head', branch: 'main', watchedPaths: ['link'], files: [{ path: 'link', status: 'modified' }] };
  const revision = statusRevision(() => status, 'link');
  const initial = revision();
  status = { head_oid: 'head', branch: 'main', watchedPaths: [], files: [] };
  assert.equal(revision(), initial);
  status = { head_oid: 'head', branch: 'main', watchedPaths: ['link'], files: [{ path: 'link', status: 'modified' }] };
  assert.equal(revision(), initial);
  status = { head_oid: 'head', branch: 'main', watchedPaths: ['link'], files: [] };
  assert.notEqual(revision(), initial);
});

test('a watched path error affects only its link and survives an older poll', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const errors = [], notifications = [];
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event },
    window: { showErrorMessage: message => errors.push(message) } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event },
    { async readWorkingSymlink(base, relative) { return { target: `target-${relative}` }; } });
  t.after(() => links.dispose());
  let status = { head_oid: 'head', branch: 'main', watchedPaths: ['one', 'two'], files: [], watched_errors: [] };
  const first = await links.create(uri('/repo/one'), '/repo', 'one', () => true, undefined,
    statusRevision(() => status, 'one'));
  const second = await links.create(uri('/repo/two'), '/repo', 'two', () => true, undefined,
    statusRevision(() => status, 'two'));
  status.watchTokens = new Map([['one', links.watchTokenFor('/repo', 'one')], ['two', links.watchTokenFor('/repo', 'two')]]);
  opened.fire({ uri: first });
  opened.fire({ uri: second });
  await links.refresh();
  links.onDidChange(value => notifications.push(value.toString()));
  status = { ...status, watched_errors: [{ path: 'one', code: 'outside_workspace', message: 'Path leaves the workspace' }] };
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, [first.toString()]);
  assert.match(errors[0], /one.*leaves the workspace/i);
  await assert.rejects(links.provideTextDocumentContent(first), /leaves the workspace/i);
  assert.equal(await links.provideTextDocumentContent(second), 'target-two');
  status = { ...status, watchedPaths: [], watched_errors: [] };
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, [first.toString()]);
  await assert.rejects(links.provideTextDocumentContent(first), /leaves the workspace/i);
  status = { ...status, watchedPaths: ['one', 'two'], watched_errors: [] };
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, [first.toString(), first.toString()]);
  assert.equal(await links.provideTextDocumentContent(first), 'target-one');
});

test('reopening a restored link ignores errors from its previous watch', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event },
    { async readWorkingSymlink() { return { target: 'restored' }; } });
  t.after(() => links.dispose());
  let status = { head_oid: 'head', branch: 'main', watchedPaths: [], files: [], watched_errors: [], watchTokens: new Map() };
  const makeDocument = () => links.create(uri('/repo/link'), '/repo', 'link', () => true, undefined,
    statusRevision(() => status, 'link'));
  const first = await makeDocument();
  opened.fire({ uri: first });
  const oldToken = links.watchTokenFor('/repo', 'link');
  status = { ...status, watchedPaths: ['link'], watchTokens: new Map([['link', oldToken]]),
    watched_errors: [{ path: 'link', code: 'outside_workspace', message: 'Path leaves the workspace' }] };
  await assert.rejects(links.provideTextDocumentContent(first), /leaves the workspace/i);
  closed.fire({ uri: first });
  const reopened = await makeDocument();
  opened.fire({ uri: reopened });
  const newToken = links.watchTokenFor('/repo', 'link');
  assert.notEqual(newToken, oldToken);
  assert.equal(await links.provideTextDocumentContent(reopened), 'restored');
  status = { ...status };
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.equal(await links.provideTextDocumentContent(reopened), 'restored');
  status = { ...status, watchTokens: new Map([['link', newToken]]), watched_errors: [] };
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.equal(await links.provideTextDocumentContent(reopened), 'restored');
});

test('unrelated repository updates do not refresh link documents', { skip: process.platform === 'win32' }, async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-link-refresh-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  const backend = { async readWorkingSymlink(base, relative) { return { target: await fs.promises.readlink(path.join(base, relative)) }; } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event }, backend);
  t.after(() => links.dispose());
  let verified = 0, revision = 'current', live = true;
  const documents = [];
  for (const name of ['one', 'two', 'three']) {
    fs.symlinkSync(`target-${name}`, path.join(root, name));
    const document = await links.create(uri(path.join(root, name)), root, name, () => live,
      async () => { verified++; return true; }, () => revision);
    opened.fire({ uri: document });
    documents.push(document);
  }
  const before = verified;
  const notifications = [];
  links.onDidChange(value => notifications.push(value.toString()));
  treeChanges.fire();
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, []);
  assert.equal(verified, before);

  const changedPath = path.join(root, 'two');
  const originalLstat = fs.lstatSync.bind(fs);
  const unchangedMetadata = originalLstat(changedPath, { bigint: true });
  t.mock.method(fs, 'lstatSync', (file, options) => file === changedPath ? unchangedMetadata : originalLstat(file, options));
  fs.unlinkSync(changedPath);
  fs.symlinkSync('new-target', changedPath);
  assert.equal(unchangedMetadata.size, BigInt('new-target'.length));
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications, [documents[1].toString()]);
  assert.equal(await links.provideTextDocumentContent(documents[1]), 'new-target');

  revision = 'changed';
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications.slice(1), documents.map(value => value.toString()));
  live = false;
  treeChanges.fire();
  t.mock.timers.tick(50);
  await settleRefresh(links);
  assert.deepEqual(notifications.slice(4), documents.map(value => value.toString()));
});

test('closing one link document during a shared probe does not invalidate its sibling', { skip: process.platform === 'win32' }, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-link-shared-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const file = path.join(root, 'link');
  fs.symlinkSync('old-target', file);
  const changes = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidCloseTextDocument: closed.event } };
  let hold = false, reads = 0, started, release;
  const ready = new Promise(resolve => { started = resolve; });
  const backend = { async readWorkingSymlink(base, relative) {
    reads++;
    if (hold) { started(); await new Promise(resolve => { release = resolve; }); }
    return { target: await fs.promises.readlink(path.join(base, relative)) };
  } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, backend);
  t.after(() => links.dispose());
  const first = await links.create(uri(file), root, 'link', () => true);
  const second = await links.create(uri(file), root, 'link', () => true);
  const notifications = [];
  links.onDidChange(value => notifications.push(value.toString()));
  fs.unlinkSync(file);
  fs.symlinkSync('new-target', file);
  hold = true;
  const before = reads;
  const refresh = links.refresh();
  await ready;
  closed.fire({ uri: first });
  hold = false;
  release();
  await refresh;
  assert.equal(reads - before, 1);
  assert.deepEqual(notifications, [second.toString()]);
  assert.equal(await links.provideTextDocumentContent(second), 'new-target');
});

test('concurrent link reads are bounded and closed documents reject pending results', async t => {
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  let blocking = false, active = 0, maximum = 0;
  const releases = [];
  const backend = { async readWorkingSymlink() {
    if (blocking) {
      active++;
      maximum = Math.max(maximum, active);
      await new Promise(resolve => releases.push(resolve));
      active--;
    }
    return { target: 'target' };
  } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event }, backend);
  t.after(() => links.dispose());
  const documents = [];
  for (let index = 0; index < 4; index++) {
    const document = await links.create(uri(`/repo/link-${index}`), '/repo', `link-${index}`, () => true);
    opened.fire({ uri: document });
    documents.push(document);
  }
  blocking = true;
  const pending = documents.map(document => links.provideTextDocumentContent(document));
  const settled = Promise.allSettled(pending);
  await new Promise(resolve => setImmediate(resolve));
  closed.fire({ uri: documents[0] });
  for (let attempt = 0; attempt < 4; attempt++) {
    for (const release of releases.splice(0)) release();
    await new Promise(resolve => setImmediate(resolve));
  }
  const results = await settled;
  assert.equal(maximum, 2);
  assert.equal(results[0].status, 'rejected');
  assert.ok(results.slice(1).every(result => result.status === 'fulfilled' && result.value === 'target'));
});

test('a stale selection cannot create or update a symlink document', async t => {
  const treeChanges = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  let release;
  const backend = { readWorkingSymlink: () => new Promise(resolve => { release = resolve; }) };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event }, backend);
  t.after(() => links.dispose());
  let current = true;
  const pending = links.create(uri('/repo/link'), '/repo', 'link', () => current);
  await new Promise(resolve => setImmediate(resolve));
  current = false;
  release({ target: 'target' });
  await assert.rejects(pending, /no longer in the workspace/i);
});
