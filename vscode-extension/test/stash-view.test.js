'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');

const repoPath = path.resolve('workspace/repo');
const identity = 'b'.repeat(64);
const oid = 'a'.repeat(40);
const oid2 = 'c'.repeat(40);

let cached;
function load() {
  if (cached) return cached;
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const vscode = {
    EventEmitter,
    FileSystemError: { NoPermissions: () => Object.assign(new Error('NoPermissions'), { code: 'NoPermissions' }) },
    Uri: { file: value => fakeUri(value, 'file', value) },
    workspace: {},
    window: { messages: [], async showInformationMessage(message) { this.messages.push(message); } },
    commands: { calls: [], async executeCommand(...args) { this.calls.push(args); } }
  };
  const loadModule = Module._load;
  let StashView;
  try {
    Module._load = function (name, ...args) { return name === 'vscode' ? vscode : loadModule.call(this, name, ...args); };
    ({ StashView } = require('../src/stash-view'));
  } finally { Module._load = loadModule; }
  cached = { vscode, StashView };
  return cached;
}

function fakeUri(value, scheme, component) {
  return { scheme, fsPath: component, path: component, query: '', fragment: '',
    toString() { return `${scheme}:${component}`; },
    with(change) { return fakeUri(value, change.scheme || scheme, change.path || component); } };
}

function fixture({ files = [], stash = { index: 0, oid, message: 'WIP' } } = {}) {
  const { vscode, StashView } = load();
  vscode.commands.calls.length = 0;
  vscode.window.messages.length = 0;
  const written = [];
  const repo = { rootUri: fakeUri(repoPath, 'file', repoPath) };
  const status = { repo_identity: identity, stashes: [stash] };
  const tree = { output: { appendLine() {} }, backend: { async validateRepositoryIdentity() { return { valid: true }; } },
    onDidChangeTreeData: new (class { constructor() { this.event = () => ({ dispose() {} }); } })().event };
  // A real LinkDocuments keeps this test on the production virtual-document
  // path: createVerified must hand back the gitpane-link UUID, not the file URI.
  const links = new LinkDocuments(vscode, tree, tree.backend);
  const create = links.createVerified.bind(links);
  links.createVerified = async (fileUri, read, ...rest) => { written.push(fileUri.fsPath); return create(fileUri, read, ...rest); };
  const provider = {
    disposed: false,
    tree: { git: { repositories: [repo] }, backendStatuses: new Map([[repo.rootUri.toString(), status]]),
      isCurrentRepository: (value, token) => token === 1, backendGeneration: 4 },
    publish() {},
    links
  };
  const calls = [];
  provider.stash = {
    async listFiles(request) { calls.push(request); return { stash_oid: request.oid, repo_identity: identity,
      message: stash.message || 'WIP', parent_oid: 'd'.repeat(40), untracked_parent_oid: null,
      files: files.map(entry => ({ source: 'tracked', status: 'M', old_path: entry.path, new_path: entry.path,
        binary: false, gitlink: false, ...entry })) }; },
    async readFile(request) { calls.push(request); return { old: { kind: 'text', text: 'old\n' }, new: { kind: 'text', text: 'new\n' } }; }
  };
  const view = new StashView(provider, provider.stash);
  const live = new Set(), ids = new Map();
  const idFor = (key, set) => { set.add(key); if (!ids.has(key)) ids.set(key, `item-${ids.size + 1}`); return ids.get(key); };
  const snapshot = () => view.snapshot(repo, repo.rootUri.toString(), 1, status.stashes, live, idFor);
  return { vscode, repo, status, provider, view, links, calls, written, snapshot,
    prune: () => view.prune(), setStash(value) { status.stashes = value; } };
}

test('stash nodes expose opaque ids while the view keeps the OID', async () => {
  const f = fixture();
  const [stash] = f.snapshot();
  assert.equal('oid' in stash, false);
  assert.equal(stash.index, 0);
  assert.equal(stash.message, 'WIP');
  assert.equal(f.view.current(stash.id).oid, oid);
  assert.equal(f.view.current(stash.id).identity, identity);
  assert.equal(f.view.isCurrent(stash.id, oid), true);
  assert.equal(f.view.isCurrent(stash.id, oid2), false);
  await f.view.load(stash.id);
  assert.deepEqual(f.calls[0], { repo: repoPath, identity, oid });
  assert.equal(f.snapshot()[0].fileState, 'empty');
});

test('renumbering and removal cannot redirect a stash request', async () => {
  const f = fixture({ files: [{ path: 'one.txt' }, { status: 'R', path: 'new.txt', old_path: 'old.txt' }] });
  const first = f.snapshot()[0];
  await f.view.load(first.id);
  assert.equal(f.snapshot()[0].fileState, 'ready');
  assert.deepEqual(f.snapshot()[0].files.map(file => file.label), ['M one.txt', 'R old.txt → new.txt']);

  f.setStash([{ index: 1, oid, message: 'WIP' }]);   // same saved object, renumbered
  f.prune();
  const renumbered = f.snapshot()[0];
  assert.notEqual(renumbered.id, first.id);
  assert.equal(renumbered.fileState, undefined);
  f.setStash([]);
  f.prune();
  const calls = f.calls.length;
  await f.view.load(first.id);
  assert.equal(f.calls.length, calls);
  assert.equal(f.view.current(first.id), undefined);
});

test('loading states, empty stashes and read failures stay visible', async () => {
  const f = fixture({ files: [{ path: 'one.txt' }] });
  const [stash] = f.snapshot();
  let reject;
  f.provider.stash.listFiles = () => new Promise((resolve, error) => { reject = error; });
  const pending = f.view.load(stash.id);
  assert.equal(f.snapshot()[0].fileState, 'loading');
  reject(Object.assign(new Error('The saved stash or file is no longer available.'), { code: 'content_not_found' }));
  await pending;
  const failed = f.snapshot()[0];
  assert.equal(failed.fileState, 'error');
  assert.equal(failed.fileError, 'The saved stash or file is no longer available. Refresh and try again.');

  f.provider.stash.listFiles = async () => ({ stash_oid: oid, repo_identity: identity, message: 'WIP',
    parent_oid: 'd'.repeat(40), untracked_parent_oid: null, files: [] });
  await f.view.load(stash.id);
  assert.equal(f.snapshot()[0].fileState, 'empty');
});

test('opening a saved file reads both stash sides and never the working tree', async t => {
  const f = fixture({ files: [{ path: 'one.txt' }] });
  t.after(() => f.links.dispose());
  const [stash] = f.snapshot();
  await f.view.load(stash.id);
  const file = f.snapshot()[0].files[0];
  await f.view.open(stash.id, file.id);
  const content = f.calls.at(-1);
  assert.deepEqual(content, { repo: repoPath, identity, oid, file: { source: 'tracked', status: 'M', path: 'one.txt',
    old_path: 'one.txt', new_path: 'one.txt', binary: false, gitlink: false, id: file.id } });
  const [command, left, right, title] = f.vscode.commands.calls.at(-1);
  assert.equal(command, 'vscode.diff');
  assert.equal(title, 'one.txt (stash@{0})');
  assert.match(left.toString(), /^gitpane-link:\/[0-9a-f-]{36}$/);
  assert.equal(await f.links.provideTextDocumentContent(left), 'old\n');
  assert.equal(await f.links.provideTextDocumentContent(right), 'new\n');
  assert.deepEqual(f.written, [path.join(repoPath, 'one.txt'), path.join(repoPath, 'one.txt')]);
});

test('binary files, unknown files and dropped stashes report visible errors', async () => {
  const f = fixture({ files: [{ path: 'one.txt' }] });
  const [stash] = f.snapshot();
  await f.view.load(stash.id);
  const file = f.snapshot()[0].files[0];
  f.provider.stash.readFile = async () => ({ old: { kind: 'text', text: 'a' }, new: { kind: 'binary', text: null } });
  await f.view.open(stash.id, file.id);
  assert.deepEqual(f.vscode.window.messages, ['Gitpane cannot show a text diff for the saved binary file one.txt.']);
  assert.equal(f.vscode.commands.calls.length, 0);

  await assert.rejects(f.view.open(stash.id, 'missing'), /saved stash file is no longer available/);
  f.setStash([]);
  f.prune();
  await assert.rejects(f.view.open(stash.id, file.id), /selected stash is no longer available/);
});
