'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');
const { createHash } = require('node:crypto');

test('webview shows workspace state and opens only currently visible changes', async () => {
  const calls = [];
  const errors = [];
  const vscode = {
    Uri: { joinPath: (...parts) => parts.join('/') },
    window: { async showErrorMessage(message) { errors.push(message); } },
    commands: { async executeCommand(...args) { calls.push(args); } }
  };
  const load = Module._load;
  let GitpaneViewProvider;
  try {
    Module._load = function (name, ...args) {
      if (name === 'vscode') return vscode;
      return load.call(this, name, ...args);
    };
    ({ GitpaneViewProvider } = require('../src/extension'));
  } finally {
    Module._load = load;
  }
  const root = path.resolve('workspace/repo');
  const worktreePath = path.join(path.dirname(root), 'worktree');
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  vscode.Uri.file = uri;
  const repo = { rootUri: uri(root), state: { HEAD: { name: 'topic', ahead: 2 }, indexChanges: [] } };
  const child = { rootUri: uri(worktreePath), state: { HEAD: { name: 'worktree-branch' }, indexChanges: [] } };
  let current = { uri: uri(path.join(root, 'file.txt')), status: 0 };
  let childCurrent = { uri: uri(path.join(worktreePath, 'child.txt')), status: 5 };
  let visible = true;
  let onChange;
  const page = records => ({ snapshot: 'fixture-snapshot', revision: createHash('sha256').update(JSON.stringify(records)).digest('hex'), cursor: 0,
    previous_cursor: null, next_cursor: null, changes: records });
  const record = (group, file, kind) => ({ group, path: file, kind,
    original_path: null, conflict_stages: null, is_submodule: false });
  const backend = {
    async beginChanges(value) {
      if (value === worktreePath) return page(childCurrent ? [record('working', 'child.txt', 'working_modified')] : []);
      return page(current ? [record('index', 'file.txt', current.status === 2 ? 'index_deleted' : 'index_modified')] : []);
    },
    async releaseChanges() {}
  };
  const tree = {
    backend,
    git: { repositories: [repo, child] }, busy: false, failedStatuses: new Set(),
    workspacePaths: [path.dirname(root)], isPathInWorkspace(value) { return value.startsWith(`${path.dirname(root)}${path.sep}`); },
    backendStatuses: new Map([[root, {
      common_dir: path.join(root, '.git'), branch: 'topic', is_dirty: true, ahead: 2, behind: 1,
      stashes: [{ index: 0, message: 'WIP on topic', oid: 'stash-commit-id' }], worktrees: [{ path: worktreePath, branch: 'worktree-branch' },
        { path: path.resolve('outside/worktree'), branch: 'outside' }], has_dirty_submodules: false,
      has_unpushed_submodules: false, file_count_lower_bound: 1
    }], [worktreePath, { common_dir: path.join(root, '.git'), file_count_lower_bound: 1 }]]),
    onDidChangeTreeData(handler) { onChange = handler; return { dispose() {} }; },
    setVisible(value) { visible = value; },
    isVisible() { return visible; },
    repositoryToken(target) { return target === repo ? 1 : target === child ? 2 : undefined; },
    isCurrentRepository(target, token) { return visible && this.repositoryToken(target) === token; },
    workspaceWorktrees(parent) {
      return visible && parent === repo ? this.backendStatuses.get(root).worktrees.filter(item => this.isPathInWorkspace(item.path)) : [];
    },
    worktreeRepository(parent, value) {
      return visible && parent === repo && value === worktreePath &&
        this.backendStatuses.get(root).worktrees.some(item => item.path === value) ? child : undefined;
    },
    groupsFor(target) {
      const change = target === child ? childCurrent : current;
      return change ? [{ id: target === child ? 'working' : 'index', label: target === child ? 'Unstaged' : 'Staged', changes: [change] }] : [];
    },
    getChildren(node) {
      if (!node) return [{ type: 'repo', repo }];
      if (node.type === 'repo') return this.groupsFor(node.repo).map(group => ({ type: 'group', repo: node.repo, group }));
      return node.group.changes.map(change => ({ type: 'change', repo: node.repo, group: node.group.id, change }));
    },
    async refresh() { repo.state.HEAD.name = 'updated'; this.backendStatuses.get(root).branch = 'updated'; onChange(); }
  };
  let onMessage, onVisibility, onDispose;
  const snapshots = [];
  const view = {
    visible: true,
    webview: {
      postMessage(snapshot) { snapshots.push(snapshot); },
      onDidReceiveMessage(handler) { onMessage = handler; return { dispose() {} }; }
    },
    onDidChangeVisibility(handler) { onVisibility = handler; return { dispose() {} }; },
    onDidDispose(handler) { onDispose = handler; return { dispose() {} }; }
  };
  const provider = new GitpaneViewProvider(tree, '/extension');
  const menus = [];
  provider.actions = { async showMenu(kind, id) { menus.push([kind, id]); } };
  try {
    Module._load = function (name, ...args) {
      if (name === './webview/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    provider.resolveWebviewView(view);
    assert.deepEqual(view.webview.options.localResourceRoots, ['/extension/src']);
  } finally {
    Module._load = load;
  }
  try {
    await onMessage({ type: 'ready' });
    const initial = snapshots.at(-1).repositories[0];
    await onMessage({ type: 'loadChanges', id: initial.id });
    await onMessage({ type: 'loadChanges', id: initial.worktrees[0].id });
    const first = snapshots.at(-1);
    assert.equal(first.repositories[0].name, 'repo');
    assert.equal(first.repositories[0].branch, 'topic');
    assert.equal(first.repositories[0].ahead, 2);
    const stash = first.repositories[0].stashes[0];
    assert.equal(stash.index, 0);
    assert.equal(stash.message, 'WIP on topic');
    assert.notEqual(stash.id, 'stash-commit-id');
    assert.equal('oid' in stash, false);
    const file = first.repositories[0].groups[0].files[0];
    assert.equal(file.path, 'file.txt');
    assert.equal(file.status, 'M');
    assert.equal(first.repositories[0].worktrees.length, 1);
    const worktree = first.repositories[0].worktrees[0];
    assert.equal(provider.resolveWorktree(worktree.id).branch, 'worktree-branch');
    assert.equal(worktree.available, true);
    const childFile = worktree.groups[0].files[0];
    assert.equal(childFile.path, 'child.txt');
    assert.equal(provider.resolveChange(childFile.id).repoPath, worktreePath);
    await onMessage({ type: 'openChange', id: childFile.id });
    assert.equal(calls[0][0], 'gitpane.openChange');
    assert.equal(calls[0][1].repo, child);
    const linked = tree.backendStatuses.get(root).worktrees.shift();
    await onMessage({ type: 'openChange', id: childFile.id });
    assert.equal(calls.length, 1);
    assert.match(errors.at(-1), /no longer available/i);
    tree.backendStatuses.get(root).worktrees.unshift(linked);
    calls.length = 0;
    assert.equal(provider.resolveChange(file.id).path, 'file.txt');
    assert.equal(provider.resolveChange(file.id).isSubmodule, false);
    repo.state.submodules = [{ path: 'file.txt' }];
    assert.equal(provider.resolveChange(file.id).isSubmodule, true);
    repo.state.submodules = [];
    assert.equal(provider.resolveRepository(first.repositories[0].id).commonDir, path.join(root, '.git'));
    repo.state.HEAD.name = 'new-branch';
    Object.assign(tree.backendStatuses.get(root), { branch: '(no branch)', head_oid: null });
    assert.equal(provider.snapshot().repositories[0].branch, 'new-branch');
    repo.state.HEAD.name = 'topic';
    tree.backendStatuses.get(root).branch = 'topic';
    await onMessage({ type: 'menu', kind: 'file', id: file.id, path: '/outside' });
    assert.deepEqual(menus, [['file', file.id]]);
    const activeListener = onChange;
    const restarted = new GitpaneViewProvider(tree, '/extension');
    try {
      assert.notEqual(restarted.snapshot().repositories[0].id, first.repositories[0].id);
    } finally {
      restarted.dispose();
      visible = true;
      onChange = activeListener;
    }

    await onMessage({ type: 'refresh' });
    assert.equal(snapshots.at(-1).repositories[0].branch, 'updated');
    assert.equal(snapshots.at(-1).repositories[0].stashes[0].id, stash.id);
    assert.equal(snapshots.at(-1).repositories[0].groups[0].files[0].id, file.id);
    await onMessage({ type: 'openChange', id: 'invalid', path: '/outside' });
    assert.deepEqual(calls, []);

    current = { uri: uri(path.join(root, 'file.txt')), status: 2 };
    await onMessage({ type: 'loadChanges', id: first.repositories[0].id });
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls[0][0], 'gitpane.openChange');
    assert.equal(calls[0][1].change.status, 2);
    current = undefined;
    await onMessage({ type: 'loadChanges', id: first.repositories[0].id });
    assert.equal(provider.resolveChange(file.id), undefined);
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls.length, 1);
    onChange();
    await onMessage({ type: 'openChange', id: file.id });
    assert.equal(calls.length, 1);

    await onMessage({ type: 'openGraph', id: 'invalid', path: '/outside' });
    assert.equal(calls.length, 1);
    await onMessage({ type: 'openGraph', id: first.repositories[0].id });
    assert.equal(calls[1][0], 'gitpane.openGraph');
    assert.equal(calls[1][1], first.repositories[0].id);

    const attention = { group: 'attention', kind: 'submodule_attention', path: 'child-module',
      original_path: null, conflict_stages: null, is_submodule: true };
    backend.beginChanges = async () => page([attention]);
    Object.assign(tree.backendStatuses.get(root), { is_dirty: false, has_unpushed_submodules: false, file_count_lower_bound: 0 });
    Object.assign(tree.backendStatuses.get(worktreePath), { has_unpushed_submodules: true, file_count_lower_bound: 0 });
    await onMessage({ type: 'loadChanges', id: worktree.id });
    const warnedWorktree = snapshots.at(-1).repositories[0].worktrees[0];
    assert.equal(warnedWorktree.hasUnpushedSubmodules, true);
    assert.equal(warnedWorktree.changeCountLowerBound, 0);
    assert.equal(warnedWorktree.groups[0].label, 'Submodules needing attention');
    const warning = warnedWorktree.groups[0].files[0];
    assert.equal(warning.status, '!');
    assert.equal(provider.resolveChange(warning.id).isSubmodule, true);
    await onMessage({ type: 'openChange', id: warning.id });
    assert.equal(calls.at(-1)[0], 'gitpane.openChange');
    assert.equal(calls.at(-1)[1].record.kind, 'submodule_attention');
    calls.pop();

    view.visible = false;
    onVisibility();
    assert.equal(visible, false);
    assert.equal(provider.resolveWorktree(worktree.id), undefined);
    await onMessage({ type: 'openGraph', id: first.repositories[0].id });
    assert.equal(calls.length, 2);
    onDispose();
    assert.equal(visible, false);
  } finally {
    provider.dispose();
  }
});
