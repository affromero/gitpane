'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

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
  const backend = { async readWorkingSymlink(base, relative) { return { target: await fs.promises.readlink(path.join(base, relative)) }; } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: treeChanges.event }, backend);
  t.after(() => links.dispose());
  let current = true;
  const document = await links.create(uri(file), repo, 'link', () => current);
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
  treeChanges.fire();
  t.mock.timers.tick(50);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(notifications, [document.toString()]);
  assert.equal(await links.provideTextDocumentContent(document), 'missing-target');
  current = false;
  await assert.rejects(links.provideTextDocumentContent(document), /no longer in the workspace/i);
  closed.fire({ uri: document });
  await assert.rejects(links.provideTextDocumentContent(document), /no longer available/i);
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
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(notifications, []);
  assert.equal(verified, before);

  fs.unlinkSync(path.join(root, 'two'));
  fs.symlinkSync('new-target', path.join(root, 'two'));
  treeChanges.fire();
  t.mock.timers.tick(50);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(notifications, [documents[1].toString()]);
  assert.equal(await links.provideTextDocumentContent(documents[1]), 'new-target');

  revision = 'changed';
  treeChanges.fire();
  t.mock.timers.tick(50);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(notifications.slice(1), documents.map(value => value.toString()));
  live = false;
  treeChanges.fire();
  t.mock.timers.tick(50);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(notifications.slice(4), documents.map(value => value.toString()));
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
