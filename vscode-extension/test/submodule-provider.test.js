'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { SubmoduleProvider } = require('../src/submodule-provider');
const { LinkDocuments } = require('../src/link-documents');
const { installGitSideFixture } = require('./git-side-fixture');

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
  const node = { repo: parent, group: 'working', change: { uri: uri(childPath), status: 5 },
    record: { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
      conflict_stages: null, is_submodule: true } };
  const child = { rootUri: uri(childPath), state: { HEAD: { commit: head }, submodules: [],
    workingTreeChanges: [{ uri: uri(path.join(childPath, 'dirty.txt')), status: 5 }] }, async status() {},
    async buffer(ref) { return Buffer.from(ref); } };
  let visible = true;
  let rowVisible = true;
  let parentRecord;
  let hasParentRecord = false;
  const listeners = new Set();
  const tokens = new Map([[parentPath, 1], [childPath, 1]]);
  const revisions = new Map([[parentPath, 1], [childPath, 1]]);
  let pick = choices => choices[0];
  let response = { repo: childPath, base_oid: base, head_oid: head, child_head_oid: head,
    parent_identity: '1'.repeat(64), child_identity: '2'.repeat(64), pointer_files: [
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
  const tree = { isVisible: () => visible, isPathInWorkspace(value) {
    try { return fs.realpathSync.native(value).startsWith(fs.realpathSync.native(parentPath) + path.sep); }
    catch { return false; }
  },
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
  tree.git = git;
  let graphGuard, graphValidate;
  let scanError;
  let parentFailure;
  const backend = { inspectSubmodule: async (repo, target, group, originalPath) => {
    inspections.push({ repo, target, group, originalPath });
    if (response instanceof Error) throw response;
    return response;
  }, async status() { return { head_oid: child.state.HEAD.commit }; },
  async repoBlobMetadata(repoPath, identity, ref) {
    assert.equal(repoPath, childPath);
    assert.equal(identity, '2'.repeat(64));
    return { oid: 'c'.repeat(40), mode: 0o100644, size: 40,
      head_oid: ref === 'HEAD' ? child.state.HEAD.commit : ref.startsWith(':') ? null : ref || null };
  },
  async openChangesSession() { if (scanError) throw scanError; return { begin: async () => page(0), next: async (_id, cursor) => page(cursor),
    close() { closedSessions++; } }; },
  async verifyChanges(repo, record) {
    if (repo === parentPath && parentFailure) throw parentFailure;
    return { change: repo === childPath ? records().find(item => item.path === record.path && item.group === record.group) || null : hasParentRecord ? parentRecord : record };
  }, async validateSubmoduleChain(chain) {
    for (const link of chain) {
      const current = await this.verifyChanges(link.repo, link.record);
      const detail = await this.inspectSubmodule(link.repo, link.path, link.group, link.original_path ?? undefined);
      if (JSON.stringify(current.change) !== JSON.stringify(link.record) || detail.repo !== link.child_repo ||
          detail.base_oid !== link.base_oid || detail.head_oid !== link.head_oid || detail.child_head_oid !== link.child_head_oid ||
          detail.parent_identity !== link.parent_identity || detail.child_identity !== link.child_identity ||
          child.state.HEAD.commit !== link.child_head_oid) return { valid: false };
    }
    return { valid: true };
  } };
  installGitSideFixture(backend, repoPath => git.repositories.find(repo => repo.rootUri.fsPath === repoPath));
  const links = linkDocuments || { async createLiveBinary(value, metadata, read, guard) {
    assert.equal(await guard(), true);
    const details = await metadata();
    return { scheme: 'gitpane-blob', fsPath: value.fsPath, ref: details.ref, guard, read: () => read(details),
      toString: () => `gitpane-blob:${value.fsPath}` };
  } };
  const provider = new SubmoduleProvider(tree, backend, git, { vscode, linkDocuments: links, resolveChange: () => visible && rowVisible ? node : undefined,
    openGraph: async (repo, guard, validate) => { graphs.push(repo); graphGuard = guard; graphValidate = validate; } });
  t.after(() => provider.dispose());
  return { temporary, childPath, node, child, base, head, provider, backend, commands, errors, graphs, inspections,
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
    graphValidate() { return graphValidate(); },
    closeChild() { git.repositories = [parent]; tokens.delete(childPath); revisions.set(childPath, revisions.get(childPath) + 1); },
    openChild(repo = child) { git.repositories = [parent, repo]; tokens.set(childPath, (tokens.get(childPath) || 0) + 1);
      revisions.set(childPath, (revisions.get(childPath) || 0) + 1); },
    onOpen(callback) { open = callback; }, setRecords(value) { customRecords = value; },
    failScan(error) { scanError = error; }, failParent(error) { parentFailure = error; }, closedSessions: () => closedSessions };
}

test('opens committed rename and current dirty file comparisons using native editors', async t => {
  const f = fixture(t);
  f.setResponse({ ...f.getResponse(), repo: fs.realpathSync.native(f.childPath) });
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  await f.provider.open(f.node);
  assert.deepEqual(f.errors, []);
  assert.equal(f.commands[0][0], 'vscode.diff');
  assert.equal(f.commands[0][1].fsPath, path.join(f.childPath, 'old.txt'));
  assert.equal(f.commands[0][1].ref, f.base);
  assert.equal(f.commands[0][2].fsPath, path.join(f.childPath, 'new.txt'));
  assert.equal(f.commands[0][2].ref, f.head);
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.commands[1][1].ref, ':0');
  assert.equal(f.commands[1][2].fsPath, path.join(f.childPath, 'dirty.txt'));
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
});

test('nested deleted files use the owning repository and retain ancestor guards after paging', async t => {
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const changes = new EventEmitter();
  const links = new LinkDocuments({ EventEmitter, workspace: {}, FileType: { File: 1 }, FileChangeType: { Changed: 1 } },
    { onDidChangeTreeData: changes.event }, {});
  t.after(() => links.dispose());
  const f = fixture(t, links);
  const record = { path: 'blocked/deleted.bin', group: 'working', kind: 'working_deleted',
    original_path: null, conflict_stages: null, is_submodule: false };
  f.setRecords([record]);
  fs.writeFileSync(path.join(f.childPath, 'blocked'), 'replacement');
  const bytes = Buffer.from([0, 255, 1]);
  f.child.buffer = async (ref, file) => {
    assert.equal(ref, ':0');
    assert.equal(file, record.path);
    return bytes;
  };
  f.backend.repoBlobMetadata = async () => ({ oid: 'c'.repeat(40), mode: 0o100644, size: bytes.length, head_oid: null });
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.errors.length, 0, f.errors.at(-1));
  const [command, left, right] = f.commands.at(-1);
  assert.equal(command, 'vscode.diff');
  assert.equal(left.toString().startsWith('gitpane-blob:'), true);
  assert.equal(right.toString().startsWith('gitpane-empty:'), true);
  assert.deepEqual(await links.readFile(left), bytes);
  f.setRowVisible(false);
  assert.deepEqual(await links.readFile(left), bytes);
  f.setParentRecord({ ...f.node.record, kind: 'working_deleted' });
  await assert.rejects(links.readFile(left), /no longer|changed/i);
});

test('nested working file reads still reject blocked ancestors', async t => {
  const f = fixture(t);
  f.setRecords([{ path: 'blocked/file.txt', group: 'working', kind: 'working_modified',
    original_path: null, conflict_stages: null, is_submodule: false }]);
  fs.writeFileSync(path.join(f.childPath, 'blocked'), 'replacement');
  f.pick(choices => choices.find(choice => choice.kind === 'change') || choices.find(choice => choice.kind === 'browse'));
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /unsafe ancestor|cannot open/i);
});

test('submodule diffs use the selected pointer endpoints while guarding the live child', async t => {
  const f = fixture(t);
  const live = 'c'.repeat(40);
  f.child.state.HEAD.commit = live;
  f.node.group = 'index';
  f.node.record = { ...f.node.record, group: 'index', kind: 'index_modified' };
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

test('historical nested gitlinks open pointer diffs and keep ancestor guards', async t => {
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const changes = new EventEmitter();
  const links = new LinkDocuments({ EventEmitter, workspace: {} }, { onDidChangeTreeData: changes.event }, {
    async validateRepositoryIdentity() { return { valid: true }; }
  });
  t.after(() => links.dispose());
  const f = fixture(t, links);
  const pointer = oid => ({ kind: 'gitlink', oid, mode: 57344 });
  const before = '1'.repeat(40), after = '2'.repeat(40);
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  const cases = [
    { status: 'M', old: pointer(before), new: pointer(after) },
    { status: 'A', old: null, new: pointer(after) },
    { status: 'D', old: pointer(before), new: null }
  ];
  for (const item of cases) {
    f.setResponse({ ...f.getResponse(), pointer_files: [{ path: 'nested', status: item.status,
      is_submodule: true, gitlink: { old: item.old, new: item.new } }] });
    const previous = f.commands.length;
    await f.provider.open(f.node);
    assert.equal(f.commands.length, previous + 1, f.errors.at(-1));
    const [command, left, right] = f.commands.at(-1);
    assert.equal(command, 'vscode.diff');
    for (const [uri, side] of [[left, item.old], [right, item.new]]) {
      if (side) assert.equal(await links.provideTextDocumentContent(uri), `Subproject commit ${side.oid}\n`);
      else assert.match(uri.toString(), /^gitpane-empty:/);
    }
  }
  assert.equal(f.opened(), 3, 'only the inspected child repository is opened');
  const oldDocument = f.commands[0][1];
  f.reopenParent();
  await assert.rejects(links.provideTextDocumentContent(oldDocument), /no longer/i);
});

test('historical nested gitlinks reject changed selections and unsupported sides', async t => {
  class EventEmitter {
    constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const changes = new EventEmitter();
  let f, reopenDuringIdentityCheck = false;
  const links = new LinkDocuments({ EventEmitter, workspace: {} }, { onDidChangeTreeData: changes.event }, {
    async validateRepositoryIdentity() {
      if (reopenDuringIdentityCheck) { reopenDuringIdentityCheck = false; f.reopenChild(); }
      return { valid: true };
    }
  });
  t.after(() => links.dispose());
  f = fixture(t, links);
  const pointer = oid => ({ kind: 'gitlink', oid, mode: 57344 });
  const selected = { path: 'nested', status: 'M', is_submodule: true,
    gitlink: { old: pointer('1'.repeat(40)), new: pointer('2'.repeat(40)) } };
  f.setResponse({ ...f.getResponse(), pointer_files: [selected] });
  f.pick(choices => {
    f.setResponse({ ...f.getResponse(), pointer_files: [{ ...selected,
      gitlink: { ...selected.gitlink, new: pointer('3'.repeat(40)) } }] });
    return choices.find(choice => choice.kind === 'pointer');
  });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /no longer|changed/i);

  f.setResponse({ ...f.getResponse(), pointer_files: [selected] });
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  reopenDuringIdentityCheck = true;
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0, 'a reopened child cannot receive a pending comparison');
  assert.match(f.errors.at(-1), /no longer|changed/i);

  f.setResponse({ ...f.getResponse(), pointer_files: [{ ...selected,
    gitlink: { old: { kind: 'tree', oid: '4'.repeat(40), mode: 16384 }, new: pointer('2'.repeat(40)) } }] });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /directory/i);
  f.setResponse({ ...f.getResponse(), pointer_files: [{ ...selected, path: 'bad\0path' }] });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 0);
  assert.match(f.errors.at(-1), /invalid submodule change/i);
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

test('nested working changes traverse beyond four levels and reject stale ancestors', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-nested-submodules-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const uri = (value, scheme = 'file') => ({ fsPath: value, toString: () => `${scheme}:${value}`,
    with: changes => uri(changes.path || value, changes.scheme || scheme) });
  const head = 'b'.repeat(40);
  const repos = [];
  let current = root;
  for (let index = 0; index <= 6; index++) {
    fs.mkdirSync(current, { recursive: true });
    repos.push({ rootUri: uri(current), state: { HEAD: { commit: head }, submodules: [] },
      async buffer(ref) { return Buffer.from(ref); } });
    current = path.join(current, 'child');
  }
  fs.writeFileSync(path.join(repos.at(-1).rootUri.fsPath, 'file.txt'), 'nested working change');
  const submoduleRecord = { path: 'child', group: 'working', kind: 'working_modified',
    original_path: null, conflict_stages: null, is_submodule: true };
  const fileRecord = { path: 'file.txt', group: 'working', kind: 'working_modified',
    original_path: null, conflict_stages: null, is_submodule: false };
  const node = { repo: repos[0], group: 'working', record: submoduleRecord,
    change: { uri: uri(repos[1].rootUri.fsPath), status: 5 } };
  const tokens = new Map(repos.map(repo => [repo.rootUri.fsPath, 1]));
  let withinWorkspace = true;
  const tree = { git: { repositories: repos }, isVisible: () => true,
    isPathInWorkspace: value => withinWorkspace && value.startsWith(root + path.sep), repositoryToken: repo => tokens.get(repo.rootUri.fsPath),
    repositoryRevision: value => tokens.has(value.fsPath) ? 1 : 0,
    isCurrentRepository: (repo, token) => token === tokens.get(repo?.rootUri.fsPath), onDidChangeTreeData: () => ({ dispose() {} }) };
  let activeSessions = 0, maxSessions = 0, stale = false, scopeEscape = false;
  let pinMode = false, browseDepth = 0, pinned;
  let fault, nestedInspected = false, removedNested = false, leafSelected = false, leafChecks = 0, removedLeaf = false, identityChanged = false;
  const backend = {
    async repoBlobMetadata(repoPath, identity, ref) {
      assert.equal(repoPath, repos.at(-1).rootUri.fsPath);
      assert.equal(identity, '2'.repeat(64));
      return { oid: 'c'.repeat(40), mode: 0o100644, size: 40, head_oid: ref === 'HEAD' ? head : ref.startsWith(':') ? null : ref || null };
    },
    async inspectSubmodule(repo, target) {
      if (fault === 'nested' && repo === repos[1].rootUri.fsPath) nestedInspected = true;
      return { repo: path.join(repo, target), base_oid: head, head_oid: head, child_head_oid: head,
        parent_identity: '1'.repeat(64), child_identity: (identityChanged && repo === root ? '3' : '2').repeat(64), pointer_files: [] };
    },
    async status() { return { head_oid: head }; },
    async verifyChanges(repo, record) {
      if (fault === 'nested' && nestedInspected && repo === repos[0].rootUri.fsPath) removedNested = true;
      if (fault === 'leaf' && leafSelected && leafChecks === 1 && repo === repos[0].rootUri.fsPath) removedLeaf = true;
      if (fault === 'leaf' && leafSelected && repo === repos.at(-1).rootUri.fsPath) leafChecks++;
      return { change: (removedNested && repo === repos[1].rootUri.fsPath) ||
        (removedLeaf && repo === repos.at(-1).rootUri.fsPath) ? null : record };
    },
    async validateSubmoduleChain(chain) {
      for (const link of chain) {
        const current = await this.verifyChanges(link.repo, link.record);
        const detail = await this.inspectSubmodule(link.repo, link.path);
        if (JSON.stringify(current.change) !== JSON.stringify(link.record) || detail.repo !== link.child_repo ||
            detail.base_oid !== link.base_oid || detail.head_oid !== link.head_oid || detail.child_head_oid !== link.child_head_oid ||
            detail.parent_identity !== link.parent_identity || detail.child_identity !== link.child_identity) {
          return { valid: false };
        }
      }
      return { valid: true };
    },
    async openChangesSession(repo) {
      activeSessions++;
      maxSessions = Math.max(maxSessions, activeSessions);
      const record = repo === repos.at(-1).rootUri.fsPath ? fileRecord : submoduleRecord;
      return { async begin() { return { snapshot: 'snapshot', revision: 'a'.repeat(64), cursor: 0,
        previous_cursor: null, next_cursor: null, changes: [record] }; }, close() { activeSessions--; } };
    }
  };
  installGitSideFixture(backend, repoPath => repos.find(repo => repo.rootUri.fsPath === repoPath));
  const commands = [], errors = [];
  const vscode = { Uri: { file: uri }, window: {
    async showQuickPick(choices) {
      if (pinMode && browseDepth >= 2 && choices.some(item => item.kind === 'browse')) return choices.find(item => item.kind === 'pin');
      const selected = choices.find(item => item.kind === 'browse') || choices.find(item => item.kind === 'change');
      if (pinMode && selected?.kind === 'browse') browseDepth++;
      if (stale && selected?.record?.path === 'file.txt') tokens.set(repos[1].rootUri.fsPath, 2);
      if (scopeEscape && selected?.record?.is_submodule) withinWorkspace = false;
      if (fault === 'leaf' && selected?.record?.path === 'file.txt') leafSelected = true;
      if (fault === 'identity' && selected?.record?.path === 'file.txt') identityChanged = true;
      return selected;
    }, async showErrorMessage(message) { errors.push(message); }
  }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const opened = [];
  const git = { repositories: repos, async openRepository(value) { opened.push(value.fsPath); return repos.find(repo => repo.rootUri.fsPath === value.fsPath); },
    toGitUri: (value, ref) => ({ fsPath: value.fsPath, ref }) };
  const links = { async createLiveBinary(value, metadata, read, guard) {
    assert.equal(await guard(), true);
    const details = await metadata();
    return { scheme: 'gitpane-blob', fsPath: value.fsPath, ref: details.ref, guard, read: () => read(details) };
  } };
  const provider = new SubmoduleProvider(tree, backend, git, { vscode, linkDocuments: links, resolveChange: () => node });
  t.after(() => provider.dispose());
  await provider.open(node);
  assert.equal(commands.length, 1, errors.at(-1));
  assert.equal(commands[0][2].fsPath, path.join(repos.at(-1).rootUri.fsPath, 'file.txt'));
  assert.equal(maxSessions, 1, 'each paging session closes before descending');
  assert.equal(activeSessions, 0);
  stale = true;
  await provider.open(node);
  assert.equal(commands.length, 1);
  assert.match(errors.at(-1), /changed|no longer/i);
  assert.equal(activeSessions, 0);
  tokens.set(repos[1].rootUri.fsPath, 1);
  stale = false;
  scopeEscape = true;
  await provider.open(node);
  assert.equal(commands.length, 1, 'workspace removal during selection prevents descent');
  assert.match(errors.at(-1), /changed|no longer/i);
  assert.equal(activeSessions, 0);
  scopeEscape = false;
  withinWorkspace = true;
  fault = 'nested';
  opened.length = 0;
  await provider.open(node);
  assert.equal(commands.length, 1);
  assert.ok(removedNested, 'ancestor validation must trigger the selected-record removal');
  assert.ok(!opened.includes(repos[2].rootUri.fsPath), 'a disappeared nested selection cannot open its repository');
  assert.match(errors.at(-1), /changed|no longer/i);
  fault = 'leaf';
  removedNested = false;
  await provider.open(node);
  assert.ok(removedLeaf, 'the leaf must disappear during final ancestor validation');
  assert.equal(commands.length, 1, 'a disappeared leaf cannot open a diff');
  assert.match(errors.at(-1), /changed|no longer/i);
  fault = 'identity';
  removedLeaf = false;
  await provider.open(node);
  assert.ok(identityChanged, 'repository identity must change while the leaf is selected');
  assert.equal(commands.length, 1, 'replaced repository metadata cannot open a diff');
  assert.match(errors.at(-1), /changed|no longer/i);
  fault = undefined;
  identityChanged = false;
  pinMode = true;
  provider.pins = { noteOwned() {}, async pin(rootPath, links, guard, validate) {
    assert.equal(guard(), true);
    assert.equal(await validate(), true);
    pinned = { rootPath, links };
  } };
  await provider.open(node);
  assert.equal(pinned.rootPath, repos[0].rootUri.fsPath);
  assert.equal(pinned.links.length, 3);
  assert.equal(activeSessions, 0, 'nested pin closes each paging session');
  assert.equal(commands.length, 1, 'pinning does not open a diff or terminal');
});

test('a nested deleted gitlink opens when a regular file blocks its ancestor', async t => {
  const pointer = 'c'.repeat(40);
  const links = {
    async createVerified(target, read, verify, current) {
      assert.equal(await verify(), true);
      assert.equal(current(), true);
      assert.equal(await read(), `Subproject commit ${pointer}\n`);
      return target.with({ scheme: 'gitpane-link' });
    }
  };
  const f = fixture(t, links);
  fs.writeFileSync(path.join(f.childPath, 'blocked'), 'replacement file\n');
  const record = { path: 'blocked/nested', group: 'working', kind: 'working_deleted',
    original_path: null, conflict_stages: null, is_submodule: true };
  f.setRecords([record]);
  f.backend.submodulePointerCompare = async () => ({ repo_identity: '2'.repeat(64), head_oid: f.head,
    record, sides: { index: { path: record.path, kind: 'gitlink', mode: 57344, oid: pointer }, working: null } });
  f.backend.submodulePointerContent = async () => ({ content: `Subproject commit ${pointer}\n` });
  f.pick(choices => choices.find(choice => choice.kind === 'browse') || choices.find(choice => choice.record));
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 1, f.errors.at(-1));
  assert.equal(f.commands[0][0], 'vscode.diff');
  assert.equal(f.commands[0][1].toString(), `gitpane-link:${path.join(f.childPath, 'blocked', 'nested')}`);
  assert.equal(f.commands[0][2].toString(), `gitpane-empty:${path.join(f.childPath, 'blocked', 'nested')}`);
  assert.deepEqual(f.errors, []);

  f.pick(choices => {
    const selected = choices.find(choice => choice.record);
    if (selected) f.reopenChild();
    return choices.find(choice => choice.kind === 'browse') || selected;
  });
  await f.provider.open(f.node);
  assert.equal(f.commands.length, 1, 'a reopened child must not receive the stale diff');
  assert.match(f.errors.at(-1), /changed|no longer/i);
});

test('submodule conflicts report the backend error without opening a diff', async t => {
  const f = fixture(t);
  f.node.group = 'merge';
  f.node.record = { ...f.node.record, group: 'merge', kind: 'conflicted',
    conflict_stages: { base: 'child', ours: 'child', theirs: 'child' } };
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
  f.setResponse({ repo: path.join(f.temporary, 'outside'), base_oid: null, head_oid: f.head, child_head_oid: f.head,
    parent_identity: '1'.repeat(64), child_identity: '2'.repeat(64), pointer_files: [] });
  await f.provider.open(f.node);
  assert.match(f.errors[1], /no longer in the workspace/i);
  assert.equal(f.opened(), 0);
});

test('submodule identity fingerprints are required and validated before opening a repository', async t => {
  const f = fixture(t);
  const valid = f.getResponse();
  const olderBackend = { ...valid };
  delete olderBackend.parent_identity;
  f.setResponse(olderBackend);
  await f.provider.open(f.node);
  assert.match(f.errors.at(-1), /update.*backend/i);
  f.setResponse({ ...valid, parent_identity: 'forged' });
  await f.provider.open(f.node);
  assert.match(f.errors.at(-1), /invalid submodule details/i);
  assert.equal(f.opened(), 0);
});

test('a canonical submodule remains inspectable when VS Code omits it and backend still validates it', async t => {
  const f = fixture(t);
  f.node.repo.state.submodules = [];
  f.node.record = { ...f.node.record, is_submodule: true };
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

test('submodule graph validation detects pointer changes without a tree event', async t => {
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.equal(await f.graphValidate(), true);
  const response = f.getResponse();
  f.setResponse({ ...response, base_oid: 'c'.repeat(40) });
  assert.equal(await f.graphValidate(), false);
  assert.equal(f.graphCurrent(), false);
  f.setResponse(response);
  await f.provider.open(f.node);
  f.setResponse({ ...response, child_identity: '3'.repeat(64) });
  assert.equal(await f.graphValidate(), false, 'repository metadata replacement invalidates the graph');
});

test('temporary ancestor validation errors leave graph guards recoverable', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = fixture(t);
  const record = { path: 'child', group: 'working', kind: 'working_modified', original_path: null,
    conflict_stages: null, is_submodule: true };
  f.node.record = record;
  f.setParentRecord(record);
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  f.failParent(new Error('temporary backend failure'));
  f.notifyTreeChange();
  t.mock.timers.tick(500);
  await new Promise(resolve => setImmediate(resolve));
  assert.match(f.errors.at(-1), /temporary backend failure/i);
  assert.equal(f.graphCurrent(), true, 'temporary errors must not permanently invalidate the graph');
  f.failParent(undefined);
  assert.equal(await f.graphValidate(), true, 'the next validation can recover');
  assert.equal(f.graphCurrent(), true);
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
