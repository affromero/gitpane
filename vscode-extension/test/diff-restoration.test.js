'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { createLiveGitSide } = require('../src/git-side-documents');
const { DiffRestoration } = require('../src/diff-restoration');

class EventEmitter {
  constructor() { this.event = () => ({ dispose() {} }); }
  fire() {}
  dispose() {}
}

function uri(path, scheme = 'file') {
  return { scheme, fsPath: path, path, toString: () => `${scheme}:${path}`,
    with: change => uri(change.path || path, change.scheme || scheme) };
}

test('an ordinary live diff restores after the extension host restarts', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = new Map();
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-restore-'));
  const repoPath = path.join(workspace, 'repo');
  fs.mkdirSync(repoPath);
  t.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  const workspaceState = { get: key => state.get(key), async update(key, value) { state.set(key, structuredClone(value)); } };
  const warnings = [];
  const vscode = { EventEmitter, Uri: { file: value => uri(value) }, workspace: { isTrusted: true }, FileType: { File: 1 },
    window: { async showWarningMessage(message) { warnings.push(message); } } };
  const repo = { rootUri: uri(repoPath) };
  const identity = 'a'.repeat(64);
  let revision = 0, onRepository;
  const tree = { git: { repositories: [repo], onDidOpenRepository(handler) { onRepository = handler; return { dispose() {} }; } },
    backendStatuses: new Map([[repo.rootUri.toString(), { repo_identity: identity }]]),
    submoduleHierarchy: { edges: new Map() }, repositoryToken: () => 1, isCurrentRepository: () => true,
    contentRevision: () => revision, onDidChangeTreeData: () => ({ dispose() {} }),
    async refresh() { this.backendStatuses.set(repo.rootUri.toString(), { repo_identity: identity }); } };
  const scope = { roots: [workspace], rootSources: [workspace], rootIdentities: ['b'.repeat(64)] };
  const record = { group: 'working', kind: 'working_modified', path: 'file.txt', original_path: null, conflict_stages: null, is_submodule: false };
  let content = Buffer.from('old');
  let symlink = false, churn = false, workspaceChange = false;
  const backend = { async mutationSnapshot() { return scope; }, isCurrentMutation: () => true,
    async gitSideSession() { return { git: { key: 'git' } }; }, gitSideCurrent: () => true,
    async verifyChanges() {
      if (workspaceChange) { workspaceChange = false; throw Object.assign(new Error('Workspace changed'), { code: 'workspace_changed' }); }
      return { change: record };
    }, async gitSideOperation(_session, _identity, operation) {
      if (operation.kind === 'verified_content') {
        if (symlink && operation.regular_only) throw new Error('The Gitpane diff file type changed.');
        return content;
      }
      if (churn) { churn = false; revision++; }
      return { version: 1, volatile: false, replacements: 'c'.repeat(64), config: 'd'.repeat(64), results: [
        { metadata: { oid: 'e'.repeat(40), mode: 0o100644, size: content.length, head_oid: null } }
      ] };
    } };
  const first = new LinkDocuments(vscode, tree, backend, workspaceState);
  const document = await createLiveGitSide({ vscode, links: first, backend, repo, repoIdentity: identity, repoToken: 1,
    side: { gitPath: 'file.txt', ref: '' }, current: () => true, restore: { record, which: 'left' } });
  first.dispose();
  vscode.window.tabGroups = { all: [{ tabs: [{ input: { original: document } }] }],
    onDidChangeTabs: () => ({ dispose() {} }) };
  const restored = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => restored.dispose());
  tree.git.repositories = [];
  tree.backendStatuses.clear();
  restored.finishInitialization();
  const pendingDiscovery = restored.readFile(document);
  await new Promise(resolve => setImmediate(resolve));
  tree.git.repositories.push(repo);
  churn = true;
  workspaceChange = true;
  onRepository(repo);
  assert.equal((await pendingDiscovery).toString(), 'old');
  t.mock.timers.tick(61000);
  assert.equal((await restored.readFile(document)).toString(), 'old');
  content = Buffer.from('new');
  assert.equal((await restored.readFile(document)).toString(), 'new');
  symlink = true;
  await assert.rejects(restored.readFile(document), /file type changed/i);
  symlink = false;
  vscode.workspace.isTrusted = false;
  const untrusted = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => untrusted.dispose());
  untrusted.finishInitialization();
  await assert.rejects(untrusted.readFile(document), /no longer available/i);
  vscode.workspace.isTrusted = true;
  const changedScope = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => changedScope.dispose());
  changedScope.finishInitialization();
  scope.rootIdentities = ['f'.repeat(64)];
  await assert.rejects(changedScope.readFile(document), /no longer available/i);
  scope.rootIdentities = ['b'.repeat(64)];
  const failing = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => failing.dispose());
  failing.finishInitialization();
  const verify = backend.verifyChanges;
  backend.verifyChanges = async () => { throw new Error('Repository unavailable'); };
  await assert.rejects(failing.readFile(document), /repository unavailable/i);
  assert.ok(warnings.some(message => message.includes('cannot be restored')));
  backend.verifyChanges = verify;
  const repeatedlyChanging = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => repeatedlyChanging.dispose());
  repeatedlyChanging.finishInitialization();
  backend.verifyChanges = async () => { throw Object.assign(new Error('Workspace changed'), { code: 'workspace_changed' }); };
  await assert.rejects(repeatedlyChanging.readFile(document), /workspace changed/i);
  assert.ok(Object.hasOwn(state.get('gitpane.diffRestoration').entries, document.toString()));
  backend.verifyChanges = verify;
  const replaced = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => replaced.dispose());
  replaced.finishInitialization();
  tree.backendStatuses.set(repo.rootUri.toString(), { repo_identity: 'f'.repeat(64) });
  await assert.rejects(replaced.readFile(document), /no longer available/i);
  tree.backendStatuses.set(repo.rootUri.toString(), { repo_identity: identity });
  const typeChanged = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => typeChanged.dispose());
  typeChanged.finishInitialization();
  const originalRead = backend.gitSideOperation;
  backend.gitSideOperation = async (...args) => {
    const result = await originalRead(...args);
    if (args[2].kind === 'metadata') result.results[0].metadata.mode = 0o120000;
    return result;
  };
  await assert.rejects(typeChanged.readFile(document), /file type changed/i);
  backend.gitSideOperation = originalRead;
  let onTabs, entered, release;
  vscode.window.tabGroups = { all: [], onDidChangeTabs(handler) { onTabs = handler; return { dispose() {} }; } };
  const closing = new LinkDocuments(vscode, tree, backend, workspaceState);
  t.after(() => closing.dispose());
  closing.finishInitialization();
  const started = new Promise(resolve => { entered = resolve; });
  const originalOperation = backend.gitSideOperation;
  backend.gitSideOperation = async (...args) => {
    if (args[2].kind === 'metadata') { entered(); await new Promise(resolve => { release = resolve; }); }
    return originalOperation(...args);
  };
  const pending = closing.readFile(document);
  await started;
  onTabs({ closed: [{ input: { original: document } }] });
  release();
  await assert.rejects(pending, /no longer|changed/i);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(Object.hasOwn(state.get('gitpane.diffRestoration').entries, document.toString()), false);
  backend.gitSideOperation = originalOperation;
});

test('closing an unopened restored tab removes its saved descriptor', async t => {
  const document = uri('/opaque/file.txt', 'gitpane-blob');
  const state = new Map([['gitpane.diffRestoration', { version: 1, entries: { [document.toString()]: { saved: true } } }]]);
  const workspaceState = { get: key => state.get(key), async update(key, value) { state.set(key, structuredClone(value)); } };
  let onTabs;
  const vscode = { EventEmitter, workspace: {}, window: { tabGroups: { all: [],
    onDidChangeTabs(handler) { onTabs = handler; return { dispose() {} }; } } } };
  const tree = { onDidChangeTreeData: () => ({ dispose() {} }) };
  const docs = new LinkDocuments(vscode, tree, {}, workspaceState);
  t.after(() => docs.dispose());
  onTabs({ closed: [{ input: { original: document } }] });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(state.get('gitpane.diffRestoration').entries, {});
});

test('disposing while repository discovery is pending ends the read', async t => {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-restore-wait-'));
  t.after(() => fs.rmSync(workspace, { recursive: true, force: true }));
  const document = uri('/opaque/file.txt', 'gitpane-blob');
  const record = { group: 'working', kind: 'working_modified', path: 'file.txt', original_path: null,
    conflict_stages: null, is_submodule: false };
  const descriptor = { repo: workspace, identity: 'a'.repeat(64), record, which: 'left', scope: '[]' };
  const state = new Map([['gitpane.diffRestoration', { version: 1, entries: { [document.toString()]: descriptor } }]]);
  const vscode = { EventEmitter, workspace: { isTrusted: true } };
  const tree = { git: { repositories: [], onDidOpenRepository: () => ({ dispose() {} }) },
    onDidChangeTreeData: () => ({ dispose() {} }) };
  const docs = new LinkDocuments(vscode, tree, {}, { get: key => state.get(key), async update(key, value) { state.set(key, value); } });
  docs.finishInitialization();
  const read = docs.readFile(document);
  await new Promise(resolve => setImmediate(resolve));
  docs.dispose();
  await assert.rejects(read, /no longer available/i);
  assert.deepEqual(state.get('gitpane.diffRestoration').entries[document.toString()], descriptor);
});

test('unsupported saved restoration data is preserved and reported', async () => {
  const saved = { version: 2, entries: { future: true } };
  let written = false;
  const restoration = new DiffRestoration({}, {}, {}, {}, {
    get: () => saved, async update() { written = true; }
  });
  await assert.rejects(restoration.forget('old'), /unsupported version/i);
  assert.equal(written, false);
  assert.deepEqual(saved, { version: 2, entries: { future: true } });
});
