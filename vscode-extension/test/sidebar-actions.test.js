'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { SidebarActions } = require('../src/sidebar-actions');

function fixture() {
  const root = path.resolve('workspace/repo');
  const commonDir = path.join(root, '.git');
  const uri = { fsPath: path.join(root, 'file.txt') };
  const node = { change: { uri } };
  let file = { repoPath: root, commonDir, path: 'file.txt', group: 'working', status: 5, uri, node };
  let repo = { repoPath: root, commonDir };
  let worktree = { repoPath: root, commonDir, path: path.resolve('workspace/linked'), branch: 'topic' };
  let quickPick = choices => choices[0];
  let warning = () => 'Discard';
  const inputs = [];
  const calls = [];
  const errors = [];
  let beforeValidation = () => {};
  let preview = async () => ({ token: 'a'.repeat(64) });
  let stat = async () => ({});
  let failure;
  let refreshFailure;
  const vscode = {
    window: {
      showQuickPick: async choices => quickPick(choices),
      showWarningMessage: async (message, options, button) => { calls.push(['confirm', message, options]); return warning(button); },
      showInputBox: async () => inputs.shift(),
      showErrorMessage: async message => { errors.push(message); },
      showInformationMessage: async message => { calls.push(['info', message]); }
    },
    commands: { executeCommand: async (...args) => { calls.push(args); } },
    env: { clipboard: { writeText: async text => { calls.push(['copy', text]); } } },
    Uri: { file: fsPath => ({ fsPath }) },
    workspace: { fs: { stat: (...args) => stat(...args) } }
  };
  const actions = new SidebarActions({
    vscode,
    resolveChange: id => id === 'file' ? file : undefined,
    resolveRepository: id => id === 'repo' ? repo : undefined,
    resolveWorktree: id => id === 'worktree' ? worktree : undefined,
    mutations: { async execute(method, params, options) {
      await beforeValidation();
      if (!await options.validateTarget()) throw Object.assign(new Error('stale'), { code: 'workspace_changed' });
      calls.push(['mutation', method, params, options.lockKey]);
      if (failure) throw failure;
      if (method === 'submodule_action') return { completed: ['child'], skipped: ['missing'], ...(params.operation === 'pull_with_submodules' ? { parent_pulled: true } : {}) };
    } },
    refresh: async () => { calls.push(['refresh']); if (refreshFailure) throw refreshFailure; },
    onWorktreeCreated: async target => { calls.push(['openWorktree', target]); },
    reviewBranch: async id => { calls.push(['reviewBranch', id]); },
    openGithub: async id => { calls.push(['openGithub', id]); },
    openSession: async id => { calls.push(['openSession', id]); },
    openTerminal: async id => { calls.push(['openTerminal', id]); },
    openLauncher: async id => { calls.push(['openLauncher', id]); },
    previewFileAction: (...args) => preview(...args)
  });
  return {
    actions, root, commonDir, uri, node, calls, errors, inputs,
    setFile(value) { file = value; }, getFile() { return file; },
    setRepo(value) { repo = value; }, getRepo() { return repo; },
    setWorktree(value) { worktree = value; }, getWorktree() { return worktree; },
    pick(fn) { quickPick = fn; }, confirm(fn) { warning = fn; },
    setFailure(value) { failure = value; }, setRefreshFailure(value) { refreshFailure = value; },
    beforeValidation(fn) { beforeValidation = fn; },
    setPreview(fn) { preview = fn; },
    setStat(fn) { stat = fn; }
  };
}

const action = (action, id) => ({ type: 'action', action, id });

test('file menu offers applicable actions and executes stage or unstage using the selected row', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'stage'));
  await f.actions.showMenu('file', 'file');
  assert.deepEqual(f.calls, [
    ['mutation', 'file_action', { repo: f.root, path: 'file.txt', operation: 'stage' }, f.commonDir], ['refresh']
  ]);
  f.calls.length = 0;
  f.setFile({ ...f.getFile(), group: 'index', status: 0 });
  f.pick(choices => choices.find(choice => choice.action === 'unstage'));
  await f.actions.showMenu('file', 'file');
  assert.equal(f.calls[0][2].operation, 'unstage');
  assert.equal(f.calls[1][0], 'refresh');
});

test('discard and untracked deletion require confirmation describing their effects', async () => {
  const f = fixture();
  f.confirm(() => undefined);
  await f.actions.handle(action('discard', 'file'));
  assert.match(f.calls[0][1], /staged and working changes.*may be deleted/i);
  assert.equal(f.calls.length, 1);
  f.calls.length = 0;
  f.setFile({ ...f.getFile(), group: 'untracked', status: 7 });
  f.confirm(button => button);
  await f.actions.handle(action('discard', 'file'));
  assert.match(f.calls[0][1], /delete untracked file/i);
  assert.equal(f.calls[1][2].operation, 'delete_untracked');
  assert.equal(f.calls[1][2].expected_token, 'a'.repeat(64));
  assert.equal(f.calls[2][0], 'refresh');
});

test('confirmation requires a valid current file identity before a destructive action', async () => {
  const f = fixture();
  f.setPreview(async () => ({ token: 'invalid' }));
  await f.actions.handle(action('discard', 'file'));
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /invalid file confirmation/);
  f.setPreview(async () => { f.setFile(undefined); return { token: 'a'.repeat(64) }; });
  await f.actions.handle(action('discard', 'file'));
  assert.deepEqual(f.calls, []);
});

test('open, reveal and copy use only the current resolved file', async () => {
  const f = fixture();
  for (const name of ['open', 'reveal', 'copy']) await f.actions.handle(action(name, 'file'));
  assert.deepEqual(f.calls, [
    ['gitpane.openChange', f.node], ['revealInExplorer', f.uri], ['copy', path.join(f.root, 'file.txt')]
  ]);
  await f.actions.handle({ ...action('copy', 'forged'), path: '/outside' });
  f.setFile({ ...f.getFile(), path: '../outside' });
  await f.actions.handle(action('copy', 'file'));
  assert.equal(f.calls.length, 3);
});

test('file menu opens the working file separately from its changes', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'openFile'));
  await f.actions.showMenu('file', 'file');
  assert.deepEqual(f.calls, [['vscode.open', f.uri]]);
  f.calls.length = 0;
  f.setStat(async () => { throw new Error('file missing'); });
  await f.actions.handle(action('openFile', 'file'));
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /file is unavailable.*file missing/i);
  f.setStat(async () => { f.setFile(undefined); return {}; });
  await f.actions.handle(action('openFile', 'file'));
  assert.deepEqual(f.calls, []);
});

test('repository menu reveals and copies the current workspace repository path', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'revealRepo'));
  await f.actions.showMenu('repo', 'repo');
  f.pick(choices => choices.find(choice => choice.action === 'copyRepo'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['revealInExplorer', { fsPath: f.root }], ['copy', f.root]]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo(undefined); return choices.find(choice => choice.action === 'copyRepo'); });
  await f.actions.showMenu('repo', 'repo');
  await f.actions.handle(action('revealRepo', 'forged'));
  assert.deepEqual(f.calls, []);
});

test('a changed or removed row is not acted on after a menu or confirmation', async () => {
  const f = fixture();
  f.pick(choices => { f.setFile({ ...f.getFile(), status: 6 }); return choices.find(choice => choice.action === 'stage'); });
  await f.actions.showMenu('file', 'file');
  assert.deepEqual(f.calls, []);
  f.confirm(button => { f.setFile(undefined); return button; });
  await f.actions.handle(action('discard', 'file'));
  assert.equal(f.calls.length, 1);
});

test('a target changed during the worker handshake fails validation', async () => {
  const f = fixture();
  f.confirm(button => button);
  f.beforeValidation(() => f.setFile(undefined));
  await f.actions.handle(action('discard', 'file'));
  assert.equal(f.calls.some(call => call[0] === 'mutation'), false);
  assert.equal(f.calls.at(-1)[0], 'refresh');
  assert.match(f.errors[0], /stale/);
});

test('worktree creation and removal use current opaque targets and confirm removal', async () => {
  const f = fixture();
  const target = path.resolve('workspace/new-worktree');
  f.inputs.push('new-branch', target);
  await f.actions.handle(action('createWorktree', 'repo'));
  assert.deepEqual(f.calls[0], ['mutation', 'worktree_create', { repo: f.root, branch: 'new-branch', path: target }, f.commonDir]);
  assert.deepEqual(f.calls[1], ['openWorktree', target]);
  assert.deepEqual(f.calls[2], ['refresh']);
  f.calls.length = 0;
  f.confirm(button => button);
  await f.actions.showMenu('worktree', 'worktree');
  assert.match(f.calls[0][1], /remove worktree/i);
  assert.deepEqual(f.calls[1], ['mutation', 'worktree_remove', { repo: f.root, path: f.getWorktree().path }, f.commonDir]);
});

test('repository sync runs only the selected action for the current branch', async () => {
  const f = fixture();
  const head = 'a'.repeat(40);
  f.setRepo({ ...f.getRepo(), branch: 'topic', head });
  f.pick(choices => choices.find(choice => choice.action === 'pull_rebase'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls[0], ['mutation', 'repo_sync', { repo: f.root, operation: 'pull_rebase', expected_branch: 'topic', expected_head: head }, f.commonDir]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo({ ...f.getRepo(), branch: 'other' }); return choices.find(choice => choice.action === 'push'); });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, []);
});

test('detached sync sends its commit identity and unborn repositories cannot sync', async () => {
  const f = fixture();
  const head = 'b'.repeat(40);
  f.setRepo({ ...f.getRepo(), branch: undefined, head });
  await f.actions.handle(action('push', 'repo'));
  assert.deepEqual(f.calls[0], ['mutation', 'repo_sync', { repo: f.root, operation: 'push', expected_branch: null, expected_head: head }, f.commonDir]);
  f.calls.length = 0;
  f.setRepo({ ...f.getRepo(), branch: 'main', head: undefined });
  await f.actions.handle(action('push', 'repo'));
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /without a current HEAD commit/);
});

test('repository menu opens branch review for its current opaque target', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'reviewBranch'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['reviewBranch', 'repo']]);
});

test('repository menu opens GitHub for the current opaque target', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'github'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['openGithub', 'repo']]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo(undefined); return choices.find(choice => choice.action === 'github'); });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, []);
});

test('repository menu opens sessions for its current opaque target', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'session'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['openSession', 'repo']]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo(undefined); return choices.find(choice => choice.action === 'session'); });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, []);
});

test('repository menu opens a terminal only for the current repository', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'openTerminal'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['openTerminal', 'repo']]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo(undefined); return choices.find(choice => choice.action === 'openTerminal'); });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, []);
});

test('repository menu opens configured launchers only for the current repository', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'openLauncher'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['openLauncher', 'repo']]);
  f.calls.length = 0;
  f.pick(choices => { f.setRepo(undefined); return choices.find(choice => choice.action === 'openLauncher'); });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, []);
});

test('submodule actions use displayed HEAD and report skipped direct modules', async () => {
  const f = fixture();
  const head = 'c'.repeat(40);
  f.setRepo({ ...f.getRepo(), branch: 'main', head });
  f.pick(choices => choices.find(choice => choice.action === 'updateSubmodules'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls[0], ['mutation', 'submodule_action', { repo: f.root, operation: 'update', expected_branch: 'main', expected_head: head }, f.commonDir]);
  assert.match(f.calls[1][1], /updated 1 submodule.*skipped missing/i);
  assert.equal(f.calls[2][0], 'refresh');
  f.calls.length = 0;
  f.pick(choices => choices.find(choice => choice.action === 'syncSubmodules'));
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls[0][2].operation, 'sync');
  f.calls.length = 0;
  f.pick(choices => choices.find(choice => choice.action === 'pullSubmodules'));
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls[0][2].operation, 'pull_latest');
  assert.match(f.calls[1][1], /pulled 1 direct submodule/i);
  f.calls.length = 0;
  f.pick(choices => choices.find(choice => choice.action === 'pullWithSubmodules'));
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls[0][2].operation, 'pull_with_submodules');
  assert.match(f.calls[1][1], /pulled repository.*updated 1 submodule/i);
});

test('a failed or uncertain mutation is reported and refreshes even if refresh also fails', async () => {
  const f = fixture();
  f.setFailure(Object.assign(new Error('connection lost'), { code: 'outcome_unknown' }));
  f.setRefreshFailure(new Error('refresh failed'));
  await f.actions.handle(action('stage', 'file'));
  assert.equal(f.calls[1][0], 'refresh');
  assert.match(f.errors[0], /outcome is unknown.*connection lost/i);
});

test('invalid actions and forged row ids never invoke mutations', async () => {
  const f = fixture();
  await f.actions.showMenu('repo', 'forged');
  await f.actions.handle(action('stage', 'forged'));
  await f.actions.handle(action('delete_untracked', 'file'));
  await f.actions.handle(action('unstage', 'file'));
  assert.deepEqual(f.calls, []);
});
