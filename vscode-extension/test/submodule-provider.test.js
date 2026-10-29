'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { SubmoduleProvider } = require('../src/submodule-provider');
const { LinkDocuments } = require('../src/link-documents');

function fixture(t, linkDocuments) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-submodule-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const parentPath = path.join(temporary, 'parent');
  const childPath = path.join(parentPath, 'child');
  fs.mkdirSync(childPath, { recursive: true });
  fs.writeFileSync(path.join(childPath, 'dirty.txt'), 'working contents');
  const uri = (value, scheme = 'file') => ({ fsPath: value, toString: () => `${scheme}:${value}`,
    with: changes => uri(changes.path || value, changes.scheme || scheme) });
  const base = 'a'.repeat(40), head = 'b'.repeat(40);
  const parent = { rootUri: uri(parentPath), state: { submodules: [{ path: 'child' }] } };
  const node = { repo: parent, group: 'working', change: { uri: uri(childPath), status: 5 } };
  const child = { rootUri: uri(childPath), state: { HEAD: { commit: head }, submodules: [],
    workingTreeChanges: [{ uri: uri(path.join(childPath, 'dirty.txt')), status: 5 }] }, async status() {} };
  let visible = true;
  let rowVisible = true;
  let parentRecord;
  let hasParentRecord = false;
  const listeners = new Set();
  const tokens = new Map([[parentPath, 1], [childPath, 1]]);
  const revisions = new Map([[parentPath, 1], [childPath, 1]]);
  let pick = choices => choices[0];
  let response = { repo: childPath, base_oid: base, head_oid: head, child_head_oid: head, pointer_files: [
    { status: 'R', path: 'new.txt', old_path: 'old.txt', is_submodule: false }
  ] };
  let opens = 0;
  let customRecords;
  let closedSessions = 0;
  const records = () => customRecords || child.state.workingTreeChanges.map(change => ({
    path: path.relative(childPath, change.uri.fsPath).split(path.sep).join('/'),
    group: 'working', kind: 'working_modified', original_path: null, conflict_stages: null, is_submodule: false
  }));
  const page = cursor => {
    const all = records();
    return { snapshot: 'snapshot', revision: 'a'.repeat(64), cursor,
      previous_cursor: cursor ? 0 : null, next_cursor: cursor + 128 < all.length ? cursor + 128 : null,
      changes: all.slice(cursor, cursor + 128) };
  };
  const commands = [], errors = [], graphs = [], inspections = [];
  const tree = { isVisible: () => visible, isPathInWorkspace: value => value.startsWith(parentPath + path.sep),
    onDidChangeTreeData(handler) { listeners.add(handler); return { dispose: () => listeners.delete(handler) }; },
    changed() { for (const handler of listeners) handler(); },
    repositoryToken: repo => tokens.get(repo?.rootUri.fsPath),
    repositoryRevision: value => revisions.get(value.fsPath) || 0,
    isCurrentRepository: (repo, token) => visible && token !== undefined && token === tokens.get(repo?.rootUri.fsPath) };
  const vscode = { Uri: { file: uri }, window: {
    showQuickPick: async choices => pick(choices),
    showErrorMessage: async text => errors.push(text)
  }, commands: { executeCommand: async (...args) => commands.push(args) } };
  let open = async () => child;
  const git = { repositories: [parent, child], openRepository: async () => { opens++; return open(); },
    toGitUri: (value, ref) => ({ fsPath: value.fsPath, ref }) };
  let graphGuard;
  let scanError;
  const backend = { inspectSubmodule: async (repo, target, group, originalPath) => {
    inspections.push({ repo, target, group, originalPath });
    if (response instanceof Error) throw response;
    return response;
  }, async status() { return { head_oid: child.state.HEAD.commit }; },
  async openChangesSession() { if (scanError) throw scanError; return { begin: async () => page(0), next: async (_id, cursor) => page(cursor),
    close() { closedSessions++; } }; },
  async verifyChanges(repo, record) {
    return { change: repo === childPath ? records().find(item => item.path === record.path && item.group === record.group) || null : hasParentRecord ? parentRecord : record };
  } };
  const provider = new SubmoduleProvider(tree, backend, git, { vscode, linkDocuments, resolveChange: () => visible && rowVisible ? node : undefined,
    openGraph: async (repo, guard) => { graphs.push(repo); graphGuard = guard; } });
  t.after(() => provider.dispose());
  return { temporary, childPath, node, child, base, head, provider, commands, errors, graphs, inspections,
    opened: () => opens, pick(fn) { pick = fn; }, setVisible(value) { visible = value; },
    setRowVisible(value) { rowVisible = value; }, setParentRecord(value) { parentRecord = value; hasParentRecord = true; },
    notifyTreeChange() { tree.changed(); },
    attachWorktreeParent() {
      const anchorPath = path.join(temporary, 'anchor');
      fs.mkdirSync(anchorPath);
      const anchor = { rootUri: uri(anchorPath), state: {} };
      git.repositories.push(anchor);
      tokens.set(anchorPath, 1);
      node.parentWorktree = { repoKey: anchor.rootUri.toString(), repoToken: 1, path: parentPath };
      tree.workspaceWorktrees = value => value === anchor ? [{ path: parentPath }] : [];
      tree.worktreeRepository = (value, target) => value === anchor && target === parentPath ? parent : undefined;
      return () => tokens.set(anchorPath, 2);
    },
    setResponse(value) { response = value; }, getResponse() { return response; },
    reopenParent() { tokens.set(parentPath, tokens.get(parentPath) + 1); },
    reopenChild() { tokens.set(childPath, tokens.get(childPath) + 1); }, graphCurrent() { return graphGuard(); },
    closeChild() { git.repositories = [parent]; tokens.delete(childPath); revisions.set(childPath, revisions.get(childPath) + 1); },
    openChild(repo = child) { git.repositories = [parent, repo]; tokens.set(childPath, (tokens.get(childPath) || 0) + 1);
      revisions.set(childPath, (revisions.get(childPath) || 0) + 1); },
    onOpen(callback) { open = callback; }, setRecords(value) { customRecords = value; },
    failScan(error) { scanError = error; }, closedSessions: () => closedSessions };
}

test('opens committed rename and current dirty file comparisons using native editors', async t => {
  const f = fixture(t);
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  await f.provider.open(f.node);
  assert.equal(f.commands[0][0], 'vscode.diff');
  assert.equal(f.commands[0][1].fsPath, path.join(f.childPath, 'old.txt'));
  assert.equal(f.commands[0][1].ref, f.base);
  assert.equal(f.commands[0][2].fsPath, path.join(f.childPath, 'new.txt'));
  assert.equal(f.commands[0][2].ref, f.head);
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.commands[1][1].ref, '');
  assert.equal(f.commands[1][2].fsPath, path.join(f.childPath, 'dirty.txt'));
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
});

test('submodule diffs use the selected pointer endpoints while guarding the live child', async t => {
  const f = fixture(t);
  const live = 'c'.repeat(40);
  f.child.state.HEAD.commit = live;
  f.node.group = 'index';
  f.setResponse({ ...f.getResponse(), child_head_oid: live });
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  await f.provider.open(f.node);
  assert.equal(f.commands[0][1].ref, f.base);
  assert.equal(f.commands[0][2].ref, f.head);
  assert.ok(f.inspections.every(request => request.group === 'index'));

  f.setResponse({ ...f.getResponse(), base_oid: null, head_oid: f.head,
    pointer_files: [{ status: 'A', path: 'added.txt', old_path: null, is_submodule: false }] });
  await f.provider.open(f.node);
  assert.equal(f.commands[1][1].toString(), 'gitpane-empty:' + path.join(f.childPath, 'added.txt'));
  assert.equal(f.commands[1][2].ref, f.head);

  f.setResponse({ ...f.getResponse(), base_oid: f.base, head_oid: null,
    pointer_files: [{ status: 'D', path: 'deleted.txt', old_path: null, is_submodule: false }] });
  await f.provider.open(f.node);
  assert.equal(f.commands[2][1].ref, f.base);
  assert.equal(f.commands[2][2].toString(), 'gitpane-empty:' + path.join(f.childPath, 'deleted.txt'));

  f.pick(choices => {
    f.setResponse({ ...f.getResponse(), head_oid: 'd'.repeat(40) });
    return choices.find(choice => choice.kind === 'pointer');
  });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 3);
  assert.match(f.errors.at(-1), /changed/i);
});

test('staged submodule renames report the backend limitation without opening a diff', async t => {
  const f = fixture(t);
  const record = { path: 'child', group: 'index', kind: 'index_renamed', original_path: 'old-child',
    conflict_stages: null, is_submodule: true };
  f.node.group = 'index';
  f.node.record = record;
  f.setResponse(new Error('Staged submodule renames cannot be inspected yet. Inspect the rename in Source Control.'));
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.equal(f.inspections[0].group, 'index');
  assert.equal(f.inspections[0].originalPath, 'old-child');
  assert.match(f.errors.at(-1), /renames cannot be inspected/i);
});

test('submodule and regular-file type changes report the backend limitation', async t => {
  const f = fixture(t);
  f.node.group = 'index';
  f.node.record = { path: 'child', group: 'index', kind: 'index_type_changed', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.setResponse(new Error('Submodule type changes cannot be inspected yet.'));
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /type changes cannot be inspected/i);
});

test('submodule conflicts report the backend error without opening a diff', async t => {
  const f = fixture(t);
  f.node.group = 'merge';
  f.setResponse(new Error('Cannot compare conflicted submodule pointers.'));
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /conflict/i);
  assert.equal(f.inspections[0].group, 'merge');
});

test('inspects hidden submodule changes across pages and rejects stale selections', async t => {
  const f = fixture(t);
  f.child.state.workingTreeChanges = [];
  const records = Array.from({ length: 130 }, (_, index) => ({
    path: `untracked-${String(index).padStart(3, '0')}.txt`, group: 'untracked', kind: 'untracked',
    original_path: null, conflict_stages: null, is_submodule: false
  }));
  fs.writeFileSync(path.join(f.childPath, records[128].path), 'new');
  f.setRecords(records);
  let picks = 0;
  f.pick(choices => {
    assert.ok(choices.some(choice => choice.kind === 'graph'));
    assert.ok(choices.some(choice => choice.kind === 'pointer'));
    if (choices.some(choice => choice.kind === 'browse')) return choices.find(choice => choice.kind === 'browse');
    picks++;
    return picks === 1 ? choices.find(choice => choice.kind === 'next') :
      choices.find(choice => choice.record?.path === records[128].path);
  });
  await f.provider.open(f.node);
  assert.equal(f.commands[0][0], 'vscode.diff');
  assert.equal(f.commands[0][2].fsPath, path.join(f.childPath, records[128].path));
  assert.equal(f.closedSessions(), 1);

  f.pick(choices => {
    const selected = choices.find(choice => choice.record);
    if (!selected) return choices.find(choice => choice.kind === 'browse');
    f.setRecords(records.filter(record => record.path !== selected.record.path));
    return selected;
  });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 1);
  assert.match(f.errors.at(-1), /no longer available/i);
  assert.equal(f.closedSessions(), 2);
});

test('opens graph despite VS Code status and dirty scan failures', async t => {
  const f = fixture(t);
  f.child.status = async () => { throw new Error('VS Code status failed'); };
  f.failScan(new Error('Change snapshot too large'));
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
  assert.deepEqual(f.errors, []);
  f.pick(choices => choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.match(f.errors.at(-1), /snapshot too large/i);
  f.failScan(undefined);
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.commands[0][0], 'vscode.diff');
});

test('cancelling submodule selection does not start a changes scan', async t => {
  const f = fixture(t);
  f.pick(() => undefined);
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, []);
  assert.deepEqual(f.commands, []);
  assert.deepEqual(f.errors, []);
  assert.equal(f.closedSessions(), 0);
});

test('submodule symlink changes show link text and reject stale parents', { skip: process.platform === 'win32' }, async t => {
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const changes = new EventEmitter(), opened = new EventEmitter(), closed = new EventEmitter();
  const vscode = { EventEmitter, workspace: { onDidOpenTextDocument: opened.event, onDidCloseTextDocument: closed.event } };
  const links = new LinkDocuments(vscode, { onDidChangeTreeData: changes.event }, {
    async readWorkingSymlink(repo, target) { return { target: await fs.promises.readlink(path.join(repo, target)) }; }
  });
  t.after(() => links.dispose());
  const f = fixture(t, links);
  const file = path.join(f.childPath, 'dirty.txt');
  fs.unlinkSync(file);
  fs.symlinkSync('../outside-target', file);
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.commands[0][0], 'vscode.diff');
  const document = f.commands[0][2];
  assert.equal(await links.provideTextDocumentContent(document), '../outside-target');
  f.reopenParent();
  await assert.rejects(links.provideTextDocumentContent(document), /no longer in the workspace/i);
});

test('reports uninitialized submodules and refuses outside paths before opening a repository', async t => {
  const f = fixture(t);
  f.setResponse(new Error('Submodule is not initialized'));
  await f.provider.open(f.node);
  assert.match(f.errors[0], /not initialized/i);
  f.setResponse({ repo: path.join(f.temporary, 'outside'), base_oid: null, head_oid: f.head, child_head_oid: f.head, pointer_files: [] });
  await f.provider.open(f.node);
  assert.match(f.errors[1], /no longer in the workspace/i);
  assert.equal(f.opened(), 0);
});

test('a canonical submodule remains inspectable when VS Code omits it and backend still validates it', async t => {
  const f = fixture(t);
  f.node.repo.state.submodules = [];
  f.node.record = { is_submodule: true };
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
  f.setResponse(new Error('selected path is not a registered submodule'));
  await f.provider.open(f.node);
  assert.match(f.errors.at(-1), /not a registered submodule/i);
});

test('rejects a removed repository or changed HEAD after the picker', async t => {
  const f = fixture(t);
  f.pick(choices => { f.setVisible(false); return choices[0]; });
  await f.provider.open(f.node);
  assert.match(f.errors[0], /changed/i);
  assert.equal(f.commands.length, 0);
  f.setVisible(true);
  f.pick(choices => { f.child.state.HEAD.commit = 'c'.repeat(40); return choices[0]; });
  await f.provider.open(f.node);
  assert.match(f.errors[1], /changed/i);
  assert.equal(f.graphs.length, 0);
});

test('rejects a reopened parent or child at the same path during selection', async t => {
  const f = fixture(t);
  f.pick(choices => { f.reopenParent(); return choices[0]; });
  await f.provider.open(f.node);
  f.pick(choices => { f.reopenChild(); return choices[0]; });
  await f.provider.open(f.node);
  assert.equal(f.graphs.length, 0);
  assert.ok(f.errors.every(message => /changed/i.test(message)));
  assert.equal(f.errors.length, 2);
});

test('an open child graph retains the parent and child incarnation guard', async t => {
  const f = fixture(t);
  await f.provider.open(f.node);
  assert.equal(f.graphCurrent(), true);
  f.reopenParent();
  assert.equal(f.graphCurrent(), false);
});

test('submodule graph and picker survive paging the unchanged parent row away', async t => {
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  f.pick(choices => { f.setRowVisible(false); return choices.find(choice => choice.kind === 'graph'); });
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
  assert.equal(f.graphCurrent(), true);
  f.reopenParent();
  assert.equal(f.graphCurrent(), false);
});

test('a changed parent record cannot open a submodule graph after the picker', async t => {
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  f.pick(choices => {
    f.setRowVisible(false);
    f.setParentRecord({ ...record, kind: 'working_deleted' });
    return choices.find(choice => choice.kind === 'graph');
  });
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, []);
  assert.match(f.errors.at(-1), /changed/i);
});

test('a paged-away graph invalidates when its parent change disappears', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  f.setRowVisible(false);
  f.notifyTreeChange();
  t.mock.timers.tick(500);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.graphCurrent(), true);
  f.setParentRecord(null);
  f.notifyTreeChange();
  t.mock.timers.tick(500);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.graphCurrent(), false);
  assert.deepEqual(f.errors, []);
});

test('a submodule graph closes its guard when its owning worktree is reopened', async t => {
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  const reopenAnchor = f.attachWorktreeParent();
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
  f.setRowVisible(false);
  assert.equal(f.graphCurrent(), true);
  reopenAnchor();
  assert.equal(f.graphCurrent(), false);
});

test('a changed parent pointer cannot open an outdated committed diff', async t => {
  const f = fixture(t);
  f.pick(choices => {
    f.setResponse({ ...f.getResponse(), base_oid: 'c'.repeat(40) });
    return choices.find(choice => choice.kind === 'pointer');
  });
  await f.provider.open(f.node);
  assert.deepEqual(f.commands, []);
  assert.match(f.errors.at(-1), /changed/i);
});

test('rejects a child closed and reopened while opening, even at the same path', async t => {
  const f = fixture(t);
  f.closeChild();
  f.onOpen(async () => {
    f.openChild();
    f.closeChild();
    f.openChild();
    return f.child;
  });
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, []);
  assert.deepEqual(f.commands, []);
  assert.match(f.errors[0], /submodule is unavailable/i);
});

test('uses the current child repository after a normal first open', async t => {
  const f = fixture(t);
  f.closeChild();
  const current = { ...f.child, state: { ...f.child.state } };
  const staleWrapper = { ...f.child, state: { ...f.child.state, HEAD: { commit: 'c'.repeat(40) } } };
  f.onOpen(async () => { f.openChild(current); return staleWrapper; });
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [current]);
  assert.deepEqual(f.errors, []);
});

test('rejects an existing child closed and reopened while opening', async t => {
  const f = fixture(t);
  f.onOpen(async () => { f.closeChild(); f.openChild(); return f.child; });
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, []);
  assert.match(f.errors[0], /submodule is unavailable/i);
});

test('rejects invalid rename paths before opening any diff', async t => {
  const f = fixture(t);
  f.setResponse({ ...f.getResponse(), pointer_files: [{ status: 'R', path: 'new', old_path: '../outside', is_submodule: false }] });
  await f.provider.open(f.node);
  assert.match(f.errors[0], /invalid submodule change/i);
  assert.equal(f.commands.length, 0);
});
