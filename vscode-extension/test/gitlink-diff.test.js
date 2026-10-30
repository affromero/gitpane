'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { openGitlinkDiff } = require('../src/gitlink-diff');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

test('historical pointer documents validate their own ancestor chain', async t => {
  const changes = new EventEmitter();
  const docs = new LinkDocuments({ EventEmitter, workspace: {} }, { onDidChangeTreeData: changes.event }, {
    async validateRepositoryIdentity() { return { valid: true }; }
  });
  t.after(() => docs.dispose());
  const commands = [];
  const vscode = { Uri: { file: uri }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const repo = { rootUri: uri(path.resolve('workspace/child')) };
  const pointer = oid => ({ kind: 'gitlink', oid, mode: 57344 });
  const file = { path: 'nested', status: 'M', gitlink: { old: pointer('1'.repeat(40)), new: pointer('2'.repeat(40)) } };
  let firstCurrent = true;
  const base = { vscode, git: {}, links: docs, repo, repoIdentity: 'a'.repeat(64), repoToken: 1, file,
    oldRef: 'a'.repeat(40), newRef: 'b'.repeat(40), current: () => true, verify: async () => true,
    title: 'nested' };
  await openGitlinkDiff({ ...base, ancestorKey: 'first', verifyAncestors: async () => firstCurrent });
  await openGitlinkDiff({ ...base, ancestorKey: 'second', verifyAncestors: async () => true });
  const first = commands[0][1], second = commands[1][1];
  firstCurrent = false;
  const invalidated = [];
  docs.onDidChange(value => invalidated.push(value.toString()));
  await docs.refresh();
  assert.deepEqual(new Set(invalidated), new Set([commands[0][1].toString(), commands[0][2].toString()]));
  await assert.rejects(docs.provideTextDocumentContent(first), /no longer/i);
  assert.equal(await docs.provideTextDocumentContent(second), `Subproject commit ${'1'.repeat(40)}\n`);
});

test('historical file sides read from their parent repository and preserve ancestor guards', async t => {
  const changes = new EventEmitter();
  const docs = new LinkDocuments({ EventEmitter, workspace: {}, FileType: { File: 1 }, FileChangeType: { Changed: 1 } }, { onDidChangeTreeData: changes.event }, {
    async validateRepositoryIdentity() { return { valid: true }; }
  });
  t.after(() => docs.dispose());
  const commands = [], reads = [];
  const vscode = { Uri: { file: uri }, commands: { async executeCommand(...args) { commands.push(args); } } };
  let duringRead = () => {};
  const repo = { rootUri: uri(path.resolve('workspace/parent')), async buffer(ref, relative) {
    reads.push([ref, relative]);
    duringRead();
    return Buffer.from([0, 255, 10]);
  } };
  const file = { path: 'nested', status: 'M', gitlink: {
    old: { kind: 'file', oid: '3'.repeat(40), mode: 33188 },
    new: { kind: 'gitlink', oid: '4'.repeat(40), mode: 57344 }
  } };
  let firstCurrent = true;
  const base = { vscode, links: docs, repo, repoIdentity: 'a'.repeat(64), repoToken: 1, file,
    oldRef: 'a'.repeat(40), newRef: 'b'.repeat(40), current: () => true, verify: async () => true,
    readHistorical: async (...args) => { reads.push(args); return { size: 3 }; }, title: 'nested' };
  await openGitlinkDiff({ ...base, ancestorKey: 'first', verifyAncestors: async () => firstCurrent });
  await openGitlinkDiff({ ...base, ancestorKey: 'second', verifyAncestors: async () => true });
  assert.deepEqual(reads[0], ['a'.repeat(40), 'nested', '3'.repeat(40), 33188]);
  assert.equal((await docs.stat(commands[0][1])).size, 3);
  assert.deepEqual(await docs.readFile(commands[0][1]), Buffer.from([0, 255, 10]));
  assert.ok(reads.some(args => args.length === 2 && args[0] === 'a'.repeat(40) && args[1] === 'nested'));
  assert.equal(await docs.provideTextDocumentContent(commands[0][2]), `Subproject commit ${'4'.repeat(40)}\n`);
  firstCurrent = false;
  const invalidated = [];
  docs.onDidChange(value => invalidated.push(value.toString()));
  docs.onDidChangeFile(values => invalidated.push(...values.map(value => value.uri.toString())));
  await docs.refresh();
  assert.deepEqual(new Set(invalidated), new Set([commands[0][1].toString(), commands[0][2].toString()]));
  await assert.rejects(docs.readFile(commands[0][1]), /no longer/i);
  await assert.rejects(docs.stat(commands[0][1]), /no longer/i);
  assert.deepEqual(await docs.readFile(commands[1][1]), Buffer.from([0, 255, 10]));
  let secondCurrent = true;
  await openGitlinkDiff({ ...base, ancestorKey: 'third', verifyAncestors: async () => secondCurrent });
  duringRead = () => { secondCurrent = false; };
  await assert.rejects(docs.readFile(commands[2][1]), /no longer/i);
});

test('binary documents reject malformed metadata and release closed image tabs', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const changes = new EventEmitter(), tabsChanged = new EventEmitter(), closed = new EventEmitter();
  const tabs = { all: [], onDidChangeTabs: tabsChanged.event };
  const docs = new LinkDocuments({ EventEmitter, workspace: { onDidCloseTextDocument: closed.event }, window: { tabGroups: tabs },
    FileType: { File: 1 }, FileChangeType: { Changed: 1 } }, { onDidChangeTreeData: changes.event }, {});
  t.after(() => docs.dispose());
  let metadata = { size: 2 }, current = true;
  const make = () => docs.createBinary(uri('/workspace/image.png'), async () => metadata,
    async () => Buffer.from([0, 255]), async () => current, () => current, 'image');
  const image = await make();
  assert.equal((await docs.stat(image)).size, 2);
  metadata = { size: -1 };
  await assert.rejects(docs.stat(image), /invalid.*metadata/i);
  await assert.rejects(docs.readFile(image), /invalid.*metadata/i);
  metadata = { size: 2 };
  tabs.all = [{ tabs: [{ input: { modified: image } }] }];
  tabsChanged.fire();
  closed.fire({ uri: image });
  assert.deepEqual(await docs.readFile(image), Buffer.from([0, 255]));
  const events = [];
  docs.onDidChangeFile(items => events.push(...items));
  current = false;
  await docs.refresh();
  assert.equal(events.length, 1);
  assert.equal(events[0].uri.toString(), image.toString());
  await assert.rejects(docs.readFile(image), /no longer/i);
  current = true;
  tabs.all = [];
  tabsChanged.fire();
  t.mock.timers.tick(100);
  await assert.rejects(docs.readFile(image), /no longer/i);
});

test('binary documents live while text or tabs reference them and release after closing', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const changes = new EventEmitter(), tabsChanged = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const tabs = { all: [], onDidChangeTabs: tabsChanged.event };
  const docs = new LinkDocuments({ EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event },
    window: { tabGroups: tabs }, FileType: { File: 1 } }, { onDidChangeTreeData: changes.event }, {});
  t.after(() => docs.dispose());
  const make = () => docs.createBinary(uri('/workspace/image.png'), async () => ({ size: 1 }),
    async () => Buffer.from([7]), async () => true, () => true, 'image');
  const image = await make();
  opened.fire({ uri: image });
  t.mock.timers.tick(60000);
  assert.deepEqual(await docs.readFile(image), Buffer.from([7]));
  tabs.all = [{ tabs: [{ input: { modified: image } }] }];
  tabsChanged.fire();
  tabs.all = [];
  tabsChanged.fire();
  assert.deepEqual(await docs.readFile(image), Buffer.from([7]));
  closed.fire({ uri: image });
  opened.fire({ uri: image });
  t.mock.timers.tick(100);
  assert.deepEqual(await docs.readFile(image), Buffer.from([7]));
  closed.fire({ uri: image });
  tabsChanged.fire();
  t.mock.timers.tick(100);
  await assert.rejects(docs.readFile(image), /no longer/i);

  const delayed = await make();
  opened.fire({ uri: delayed });
  closed.fire({ uri: delayed });
  t.mock.timers.tick(59000);
  opened.fire({ uri: delayed });
  t.mock.timers.tick(60000);
  assert.deepEqual(await docs.readFile(delayed), Buffer.from([7]));
  closed.fire({ uri: delayed });
  tabs.all = [{ tabs: [{ input: { original: delayed } }] }];
  tabsChanged.fire();
  t.mock.timers.tick(60000);
  assert.deepEqual(await docs.readFile(delayed), Buffer.from([7]));
  tabs.all = [];
  tabsChanged.fire();
  t.mock.timers.tick(100);
  await assert.rejects(docs.readFile(delayed), /no longer/i);

  const pending = await make();
  t.mock.timers.tick(30000);
  tabsChanged.fire();
  t.mock.timers.tick(30000);
  await assert.rejects(docs.readFile(pending), /no longer/i);
  const disposed = await make();
  opened.fire({ uri: disposed });
  docs.dispose();
  await assert.rejects(docs.readFile(disposed), /no longer/i);
});

test('binary reads limit concurrent Git buffers and recheck waiting documents', async t => {
  const changes = new EventEmitter();
  const docs = new LinkDocuments({ EventEmitter, workspace: {} }, { onDidChangeTreeData: changes.event }, {});
  t.after(() => docs.dispose());
  let active = 0, maxActive = 0, current = true;
  const releases = [];
  const file = await docs.createBinary(uri('/workspace/file'), async () => ({ size: 1 }), async () => {
    active++;
    maxActive = Math.max(maxActive, active);
    await new Promise(resolve => releases.push(resolve));
    active--;
    return Buffer.from('x');
  }, async () => current, () => current, 'file');
  const first = docs.readFile(file), second = docs.readFile(file), third = docs.readFile(file);
  while (releases.length < 2) await new Promise(resolve => setImmediate(resolve));
  assert.equal(maxActive, 2);
  current = false;
  releases.splice(0).forEach(resolve => resolve());
  const results = await Promise.allSettled([first, second, third]);
  assert.ok(results.every(result => result.status === 'rejected' && /no longer/i.test(result.reason.message)));
  assert.equal(maxActive, 2);
});
