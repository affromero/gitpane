'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');

test('verified submodules follow their immediate visible parent and stale proofs do not hide repositories', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-hierarchy-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const paths = ['parent', 'parent/child', 'parent/child/grand', 'parent/unrelated', 'linked', 'linked/child']
    .map(value => path.join(root, value));
  for (const value of paths) fs.mkdirSync(path.join(value, '.git'), { recursive: true });
  const uri = value => ({ scheme: 'file', fsPath: value, toString: () => value });
  const event = () => ({ dispose() {} });
  const repos = paths.map(value => ({ rootUri: uri(value), state: { onDidChange: event, submodules: [] } }));
  repos[0].state.submodules = [{ path: 'child' }, { path: 'unrelated' }];
  repos[1].state.submodules = [{ path: 'grand' }];
  repos[4].kind = 'worktree';
  repos[4].state.submodules = [{ path: 'child' }];
  const vscode = { EventEmitter: class { constructor() { this.event = event; } fire() {} dispose() {} },
    workspace: { workspaceFolders: [{ uri: uri(root) }], onDidChangeWorkspaceFolders: event } };
  const api = { repositories: repos, onDidOpenRepository: event, onDidCloseRepository: event };
  const backend = { state: { status: 'ready' }, onDidChange: event, async validateSubmodulePin(parent, chain) {
    const child = path.join(parent, chain[0].path);
    if (child === fs.realpathSync.native(paths[3])) throw Object.assign(new Error('Not registered'), { code: 'stale_target' });
    return { repos: [parent, child] };
  } };
  const original = Module._load;
  let RepositoryTree;
  try {
    Module._load = function(name, ...args) { return name === 'vscode' ? vscode : original.call(this, name, ...args); };
    ({ RepositoryTree } = require('../src/repository-tree'));
  } finally { Module._load = original; }
  const tree = new RepositoryTree(api, { appendLine() {} }, backend);
  t.after(() => tree.dispose());
  repos.forEach((repo, index) => tree.backendStatuses.set(paths[index], {
    repo_identity: String(index + 1).repeat(64), common_dir: path.join(index === 4 ? paths[0] : paths[index], '.git'),
    worktrees: index === 0 ? [{ path: paths[4], branch: 'linked' }] : []
  }));
  await tree.submoduleHierarchy.refresh();
  let view = tree.presentationView();
  assert.deepEqual(view.submodules.get(paths[0]), [paths[1]]);
  assert.deepEqual(view.submodules.get(paths[1]), [paths[2]]);
  assert.deepEqual(view.submodules.get(paths[4]), [paths[5]]);
  assert.equal(view.nested.has(paths[3]), false, 'a nested repository without a gitlink stays at the root');
  assert.equal(view.nested.has(paths[4]), true, 'the linked worktree remains under its anchor');
  const validate = backend.validateSubmodulePin;
  let release;
  backend.validateSubmodulePin = async (...args) => {
    if (!release) await new Promise(resolve => { release = resolve; });
    return validate(...args);
  };
  const refreshing = tree.submoduleHierarchy.refresh();
  assert.equal(tree.presentationView().nested.has(paths[1]), true, 'a refresh retains the previous verified relationship');
  release();
  await refreshing;
  backend.validateSubmodulePin = validate;
  tree.contentRevisions.set(paths[3], 1);
  assert.equal(tree.presentationView().nested.has(paths[1]), true, 'unrelated repository updates do not flatten the hierarchy');
  tree.hiddenRepositories = { isHidden: identity => identity?.path === fs.realpathSync.native(paths[0]) };
  view = tree.presentationView();
  assert.equal(view.nested.has(paths[1]), false, 'a child remains visible when its parent is hidden');
  assert.deepEqual(view.submodules.get(paths[1]), [paths[2]]);
  tree.hiddenRepositories = undefined;
  tree.contentRevisions.set(paths[1], 1);
  view = tree.presentationView();
  assert.equal(view.nested.has(paths[1]), true, 'a state change retains the relationship until it can be rechecked');
  repos[0].state.submodules = [];
  await tree.submoduleHierarchy.refresh();
  assert.equal(tree.presentationView().nested.has(paths[1]), false, 'a removed registration no longer groups the child');
  vscode.workspace.workspaceFolders.push({ uri: uri(paths[1]) });
  tree.updateWorkspacePaths();
  await tree.submoduleHierarchy.refresh();
  view = tree.presentationView();
  assert.equal(view.nested.has(paths[1]), false, 'an explicit workspace folder remains a root');
  assert.deepEqual(view.submodules.get(paths[1]), [paths[2]], 'an explicit root retains its own children');
});
