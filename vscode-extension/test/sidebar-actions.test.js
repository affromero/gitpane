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
  const inputOptions = [];
  const plans = [];
  let input = () => inputs.shift();
  let plan = async () => ({ path: path.resolve('workspace/repo-new-branch'), problem: null });
  const calls = [];
  const errors = [];
  let beforeValidation = () => {};
  let waitForChange = async () => true;
  let preview = async () => ({ token: 'a'.repeat(64) });
  let worktreePreview = async () => ({ repo_identity: 'a'.repeat(64), worktree_identity: 'b'.repeat(64) });
  let stat = async () => ({});
  let failure;
  let refreshFailure;
  const vscode = {
    window: {
      showQuickPick: async choices => quickPick(choices),
      showWarningMessage: async (message, options, button) => { calls.push(['confirm', message, options]); return warning(button); },
      showInputBox: async options => { inputOptions.push(options); return input(options); },
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
    waitForChange: id => waitForChange(id),
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
    refreshRepository: async (id, kind, guard) => { if (guard()) calls.push(['refreshRepository', id, kind]); },
    onWorktreeCreated: async target => { calls.push(['openWorktree', target]); },
    reviewBranch: async id => { calls.push(['reviewBranch', id]); },
    openGithub: async id => { calls.push(['openGithub', id]); },
    openSession: async id => { calls.push(['openSession', id]); },
    openTerminal: async id => { calls.push(['openTerminal', id]); },
    openLauncher: async id => { calls.push(['openLauncher', id]); },
    openWorktreeGraph: async id => { calls.push(['openWorktreeGraph', id]); },
    reviewWorktree: async id => { calls.push(['reviewWorktree', id]); },
    pinSubmodule: async id => { calls.push(['pinSubmodule', id]); },
    inspectSubmodule: async id => { calls.push(['inspectSubmodule', id]); },
    browseSubmodules: async (id, kind) => { calls.push(['browseSubmodules', id, kind]); },
    hideRepository: async (item, kind) => { calls.push(['hideRepository', item.path || item.repoPath, kind]); },
    unpinSubmodule: async id => { calls.push(['unpinSubmodule', id]); },
    planWorktree: async (...args) => { plans.push(args); return plan(...args); },
    previewFileAction: (...args) => preview(...args),
    previewWorktreeRemoval: (...args) => worktreePreview(...args)
  });
  return {
    actions, root, commonDir, uri, node, calls, errors, inputs, inputOptions, plans,
    setFile(value) { file = value; }, getFile() { return file; },
    setRepo(value) { repo = value; }, getRepo() { return repo; },
    setWorktree(value) { worktree = value; }, getWorktree() { return worktree; },
    pick(fn) { quickPick = fn; }, confirm(fn) { warning = fn; },
    setFailure(value) { failure = value; }, setRefreshFailure(value) { refreshFailure = value; },
    beforeValidation(fn) { beforeValidation = fn; },
    setWaitForChange(fn) { waitForChange = fn; },
    setInput(fn) { input = fn; },
    setPlan(fn) { plan = fn; },
    setPreview(fn) { preview = fn; },
    setWorktreePreview(fn) { worktreePreview = fn; },
    setStat(fn) { stat = fn; }
  };
}

const action = (action, id) => ({ type: 'action', action, id });

test('file actions wait for a refresh and still reject a changed selection', async () => {
  const f = fixture();
  let release, started;
  const waiting = new Promise(resolve => { started = resolve; });
  f.setWaitForChange(async () => { started(); await new Promise(resolve => { release = resolve; }); return true; });
  const opening = f.actions.handle(action('open', 'file'));
  await waiting;
  assert.deepEqual(f.calls, []);
  f.setFile(undefined);
  release();
  await opening;
  assert.deepEqual(f.calls, []);
  f.setFile({ repoPath: f.root, commonDir: f.commonDir, path: 'file.txt', group: 'working', status: 5, uri: f.uri, node: f.node });
  f.setWaitForChange(async () => true);
  await f.actions.handle(action('open', 'file'));
  assert.deepEqual(f.calls[0].slice(0, 2), ['gitpane.openChange', f.node]);
  assert.equal(f.calls[0][2](), true);
  f.setFile(undefined);
  assert.equal(f.calls[0][2](), false);
});

test('repository and unavailable worktree menus offer a scoped refresh', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'refreshRepo'));
  await f.actions.showMenu('repo', 'repo');
  f.pick(choices => choices.find(choice => choice.action === 'refreshWorktree'));
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, [['refreshRepository', 'repo', 'repo'], ['refreshRepository', 'worktree', 'worktree']]);
  f.calls.length = 0;
  f.setRepo({ repoPath: f.root, refreshOnly: true });
  f.pick(choices => { assert.deepEqual(choices.map(choice => choice.action), ['refreshRepo']); return choices[0]; });
  await f.actions.showMenu('repo', 'repo');
  await f.actions.handle(action('push', 'repo'));
  assert.deepEqual(f.calls, [['refreshRepository', 'repo', 'repo']]);
});

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

test('submodule rows offer inspection but no file mutations', async () => {
  const f = fixture();
  for (const group of ['working', 'index', 'untracked', 'merge', 'attention']) {
    f.setFile({ ...f.getFile(), group, isSubmodule: true });
    f.pick(choices => {
      assert.deepEqual(choices.map(choice => choice.action), ['open', 'openFile', 'reveal', 'copy', 'inspectSubmodule', 'pinSubmodule']);
      return choices.find(choice => choice.action === 'open');
    });
    await f.actions.showMenu('file', 'file');
    for (const name of ['stage', 'unstage', 'discard']) await f.actions.handle(action(name, 'file'));
  }
  assert.deepEqual(f.calls.map(call => call.slice(0, 2)), Array.from({ length: 5 }, () => ['gitpane.openChange', f.node]));
});

test('submodule file and pinned repository menus expose scoped pin actions', async () => {
  const f = fixture();
  f.setFile({ ...f.getFile(), isSubmodule: true });
  f.pick(choices => choices.find(choice => choice.action === 'pinSubmodule'));
  await f.actions.showMenu('file', 'file');
  f.setRepo({ ...f.getRepo(), pinned: true });
  f.pick(choices => choices.find(choice => choice.action === 'unpinSubmodule'));
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [['pinSubmodule', 'file'], ['unpinSubmodule', 'repo']]);
  f.setRepo({ ...f.getRepo(), pinned: false });
  await f.actions.handle(action('unpinSubmodule', 'repo'));
  assert.equal(f.calls.length, 2);
});

test('submodule inspection remains available from the file menu', async () => {
  const f = fixture();
  f.setFile({ ...f.getFile(), isSubmodule: true, group: 'index' });
  f.pick(choices => choices.find(choice => choice.action === 'inspectSubmodule'));
  await f.actions.showMenu('file', 'file');
  assert.deepEqual(f.calls, [['inspectSubmodule', 'file']]);
});

test('repository and available worktree menus offer submodule browsing only for current selections', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'browseSubmodules'));
  await f.actions.showMenu('repo', 'repo');
  f.setWorktree({ ...f.getWorktree(), available: true, childRepoId: 'child' });
  f.pick(choices => choices.find(choice => choice.action === 'browseWorktreeSubmodules'));
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, [['browseSubmodules', 'repo', 'repo'], ['browseSubmodules', 'worktree', 'worktree']]);
  f.pick(choices => { f.setRepo({ ...f.getRepo(), repoIdentity: 'replacement' }); return choices.find(choice => choice.action === 'browseSubmodules'); });
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls.length, 2);
  f.setWorktree({ ...f.getWorktree(), available: false });
  f.pick(choices => { assert.ok(!choices.some(choice => choice.action === 'browseWorktreeSubmodules')); });
  await f.actions.showMenu('worktree', 'worktree');
});

test('repository and worktree hide actions follow the current menu selection', async () => {
  const f = fixture();
  f.pick(choices => choices.find(choice => choice.action === 'hideRepository'));
  await f.actions.showMenu('repo', 'repo');
  f.pick(choices => choices.find(choice => choice.action === 'hideWorktree'));
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, [['hideRepository', f.root, 'repo'], ['hideRepository', f.getWorktree().path, 'worktree']]);
  f.pick(choices => { f.setWorktree({ ...f.getWorktree(), path: path.resolve('other') });
    return choices.find(choice => choice.action === 'hideWorktree'); });
  await f.actions.showMenu('worktree', 'worktree');
  assert.equal(f.calls.length, 2);
});

test('a file that becomes a submodule while its menu is open cannot be mutated', async () => {
  const f = fixture();
  f.setFile({ ...f.getFile(), isSubmodule: false });
  f.pick(choices => {
    f.setFile({ ...f.getFile(), isSubmodule: true });
    return choices.find(choice => choice.action === 'stage');
  });
  await f.actions.showMenu('file', 'file');
  assert.deepEqual(f.calls, []);
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
  assert.deepEqual(f.calls.map(call => call.slice(0, 2)), [
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
  assert.deepEqual(f.plans, [[f.root, 'new-branch']]);
  assert.equal(f.inputOptions[1].value, path.resolve('workspace/repo-new-branch'));
  assert.equal(f.inputOptions[1].validateInput(target), undefined);
  assert.match(f.inputOptions[1].validateInput('relative/path'), /absolute/i);
  assert.deepEqual(f.calls[0], ['mutation', 'worktree_create', { repo: f.root, branch: 'new-branch', path: target }, f.commonDir]);
  assert.deepEqual(f.calls[1], ['openWorktree', target]);
  assert.deepEqual(f.calls[2], ['refresh']);
  f.calls.length = 0;
  f.confirm(button => button);
  f.pick(choices => choices.find(choice => choice.action === 'removeWorktree'));
  await f.actions.showMenu('worktree', 'worktree');
  assert.match(f.calls[0][1], /remove worktree/i);
  assert.deepEqual(f.calls[1], ['mutation', 'worktree_remove', { repo: f.root, path: f.getWorktree().path,
    expected_repo_identity: 'a'.repeat(64), expected_worktree_identity: 'b'.repeat(64) }, f.commonDir]);
});

test('an unavailable configured worktree location is shown and can be replaced', async () => {
  const f = fixture();
  const outside = path.resolve('outside/repo-topic');
  const inside = path.resolve('workspace/repo-topic');
  f.setPlan(async () => ({ path: outside, problem: 'Worktree destination is outside the workspace' }));
  f.inputs.push('topic', inside);
  await f.actions.handle(action('createWorktree', 'repo'));
  assert.equal(f.inputOptions[1].value, outside);
  assert.match(f.inputOptions[1].validateInput(outside), /outside the workspace/i);
  assert.equal(f.inputOptions[1].validateInput(inside), undefined);
  assert.deepEqual(f.calls[0], ['mutation', 'worktree_create', { repo: f.root, branch: 'topic', path: inside }, f.commonDir]);

  const unchanged = fixture();
  unchanged.setPlan(async () => ({ path: outside, problem: 'Worktree destination is outside the workspace' }));
  unchanged.inputs.push('topic', outside);
  await unchanged.actions.handle(action('createWorktree', 'repo'));
  assert.deepEqual(unchanged.calls, []);
});

test('worktree creation reports invalid plans and planner errors before asking for a path', async () => {
  const invalid = [undefined, {}, { path: 'relative', problem: null },
    { path: path.resolve('workspace/repo-topic') + '\0bad', problem: null },
    { path: path.resolve('workspace/repo-topic'), problem: 5 },
    { path: path.resolve('workspace/repo-topic'), problem: '' }];
  for (const result of invalid) {
    const f = fixture();
    f.setPlan(async () => result);
    f.inputs.push('topic');
    await f.actions.handle(action('createWorktree', 'repo'));
    assert.equal(f.inputOptions.length, 1);
    assert.deepEqual(f.calls, []);
    assert.match(f.errors[0], /invalid.*worktree/i);
  }
  const f = fixture();
  f.setPlan(async () => { throw new Error('invalid branch name'); });
  f.inputs.push('bad branch');
  await f.actions.handle(action('createWorktree', 'repo'));
  assert.equal(f.inputOptions.length, 1);
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /invalid branch name/i);
});

test('an older configured backend reports how to enable worktree creation', async () => {
  const f = fixture();
  f.setPlan(async () => { throw Object.assign(new Error('unknown bridge method'), { code: 'invalid_request' }); });
  f.inputs.push('topic');
  await f.actions.handle(action('createWorktree', 'repo'));
  assert.equal(f.inputOptions.length, 1);
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /update.*backend/i);

  const branch = fixture();
  branch.setPlan(async () => { throw Object.assign(new Error('invalid branch name'), { code: 'invalid_request' }); });
  branch.inputs.push('bad branch');
  await branch.actions.handle(action('createWorktree', 'repo'));
  assert.match(branch.errors[0], /invalid branch name/i);
  assert.doesNotMatch(branch.errors[0], /update.*backend/i);
});

test('cancelled or stale worktree planning and path selection cannot create a worktree', async () => {
  const cancelledBranch = fixture();
  await cancelledBranch.actions.handle(action('createWorktree', 'repo'));
  assert.deepEqual(cancelledBranch.plans, []);
  assert.deepEqual(cancelledBranch.calls, []);

  const stalePlan = fixture();
  stalePlan.inputs.push('topic');
  stalePlan.setPlan(async () => {
    stalePlan.setRepo(undefined);
    return { path: path.resolve('workspace/repo-topic'), problem: null };
  });
  await stalePlan.actions.handle(action('createWorktree', 'repo'));
  assert.equal(stalePlan.inputOptions.length, 1);
  assert.deepEqual(stalePlan.calls, []);

  const cancelledPath = fixture();
  cancelledPath.inputs.push('topic', undefined);
  await cancelledPath.actions.handle(action('createWorktree', 'repo'));
  assert.equal(cancelledPath.inputOptions.length, 2);
  assert.deepEqual(cancelledPath.calls, []);

  const stalePath = fixture();
  stalePath.setInput(options => {
    if (!options.value) return 'topic';
    stalePath.setRepo(undefined);
    return options.value;
  });
  await stalePath.actions.handle(action('createWorktree', 'repo'));
  assert.deepEqual(stalePath.calls, []);
});

test('available worktrees offer graph and review only while their repository remains current', async () => {
  const f = fixture();
  f.setWorktree({ ...f.getWorktree(), available: true, childRepoKey: 'linked-repo', childRepoId: 'repo' });
  f.pick(choices => choices.find(choice => choice.action === 'openWorktreeGraph'));
  await f.actions.showMenu('worktree', 'worktree');
  f.pick(choices => choices.find(choice => choice.action === 'reviewWorktree'));
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, [['openWorktreeGraph', 'worktree'], ['reviewWorktree', 'worktree']]);
  f.calls.length = 0;
  let menu = 0;
  f.pick(choices => {
    const selected = menu++ === 0 ? 'worktreeRepositoryActions' : 'copyRepo';
    return choices.find(choice => choice.action === selected);
  });
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, [['copy', f.root]]);
  f.calls.length = 0;
  f.pick(choices => {
    f.setWorktree({ ...f.getWorktree(), available: false, childRepoKey: undefined });
    return choices.find(choice => choice.action === 'openWorktreeGraph');
  });
  await f.actions.showMenu('worktree', 'worktree');
  await f.actions.handle(action('reviewWorktree', 'worktree'));
  assert.deepEqual(f.calls, []);
  f.pick(choices => {
    assert.deepEqual(choices.map(choice => choice.action), ['hideWorktree', 'removeWorktree']);
    return undefined;
  });
  await f.actions.showMenu('worktree', 'worktree');
});

test('explicit worktree repository removal uses its current anchor and confirmation', async () => {
  const f = fixture();
  const removal = { repoPath: f.root, commonDir: f.commonDir, path: path.resolve('workspace/linked'),
    branch: 'topic', anchorKey: 'anchor', anchorToken: 1, targetToken: 2, commonIdentity: 'identity', workspaceFolder: true };
  f.setRepo({ ...f.getRepo(), worktreeRemoval: removal });
  f.pick(choices => choices.find(choice => choice.action === 'removeWorkspaceWorktree'));
  f.confirm(() => 'Remove');
  await f.actions.showMenu('repo', 'repo');
  assert.match(f.calls[0][1], /remove worktree/i);
  assert.match(f.calls[0][1], /removes the workspace folder on disk.*remains listed/i);
  assert.deepEqual(f.calls[1], ['mutation', 'worktree_remove', { repo: f.root, path: removal.path,
    expected_repo_identity: 'a'.repeat(64), expected_worktree_identity: 'b'.repeat(64) }, f.commonDir]);
  assert.deepEqual(f.calls[2], ['refresh']);

  f.calls.length = 0;
  f.pick(choices => {
    f.setRepo({ ...f.getRepo(), worktreeRemoval: { ...removal, anchorToken: 2 } });
    return choices.find(choice => choice.action === 'removeWorkspaceWorktree');
  });
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls, [], 'a reopened anchor cannot authorize an old menu');

  f.setRepo({ ...f.getRepo(), worktreeRemoval: removal });
  f.pick(choices => choices.find(choice => choice.action === 'removeWorkspaceWorktree'));
  f.confirm(() => { f.setRepo({ ...f.getRepo(), worktreeRemoval: undefined }); return 'Remove'; });
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls.filter(call => call[0] === 'mutation').length, 0, 'a removed registration cannot authorize confirmation');
});

test('a lone linked worktree requires matching self-removal preview and warns about the workspace folder', async () => {
  const f = fixture();
  const removal = { ...f.getRepo(), path: f.root, repoIdentity: 'a'.repeat(64), targetToken: 1,
    selfRemoval: true, workspaceFolder: true };
  f.setRepo({ ...f.getRepo(), repoIdentity: 'a'.repeat(64), worktreeRemoval: removal });
  f.pick(choices => choices.find(choice => choice.action === 'removeWorkspaceWorktree'));
  f.setWorktreePreview(async (repo, target) => {
    assert.equal(repo, f.root); assert.equal(target, f.root);
    return { repo_identity: 'a'.repeat(64), worktree_identity: 'a'.repeat(64), self_removal: true };
  });
  f.confirm(() => undefined);
  await f.actions.showMenu('repo', 'repo');
  assert.match(f.calls[0][1], /removes the workspace folder on disk.*remains listed/i);
  assert.equal(f.calls.some(call => call[0] === 'mutation'), false);
  f.calls.length = 0;
  f.confirm(() => 'Remove');
  await f.actions.showMenu('repo', 'repo');
  assert.deepEqual(f.calls[1], ['mutation', 'worktree_remove', { repo: f.root, path: f.root,
    expected_repo_identity: 'a'.repeat(64), expected_worktree_identity: 'a'.repeat(64) }, f.commonDir]);
  f.calls.length = 0;
  f.setWorktreePreview(async () => ({ repo_identity: 'b'.repeat(64), worktree_identity: 'b'.repeat(64), self_removal: true }));
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls.some(call => call[0] === 'mutation'), false);
  assert.match(f.errors.at(-1), /changed/i);
  f.setWorktreePreview(async () => ({ repo_identity: 'a'.repeat(64), worktree_identity: 'a'.repeat(64), self_removal: false }));
  await f.actions.showMenu('repo', 'repo');
  assert.equal(f.calls.some(call => call[0] === 'mutation'), false);
});

test('worktree removal rejects stale selection and invalid identity confirmations', async () => {
  for (const explicit of [false, true]) {
    const f = fixture();
    const removal = { ...f.getWorktree(), anchorToken: 1, targetToken: 2 };
    if (explicit) f.setRepo({ ...f.getRepo(), worktreeRemoval: removal });
    const selected = explicit ? action('removeWorkspaceWorktree', 'repo') : action('removeWorktree', 'worktree');
    f.confirm(() => 'Remove');
    f.setWorktreePreview(async () => {
      if (explicit) f.setRepo(undefined); else f.setWorktree(undefined);
      return { repo_identity: 'a'.repeat(64), worktree_identity: 'b'.repeat(64) };
    });
    await f.actions.handle(selected);
    assert.deepEqual(f.calls, [], 'workspace removal during preview cancels before confirmation');

    if (explicit) f.setRepo({ repoPath: f.root, commonDir: f.commonDir, worktreeRemoval: removal });
    else f.setWorktree(removal);
    f.setWorktreePreview(async () => ({ repo_identity: 'a'.repeat(64), worktree_identity: 'invalid' }));
    await f.actions.handle(selected);
    assert.deepEqual(f.calls, []);
    assert.match(f.errors.at(-1), /invalid worktree confirmation/i);

    f.setWorktreePreview(async () => ({ repo_identity: 'a'.repeat(64), worktree_identity: 'b'.repeat(64) }));
    f.beforeValidation(() => { if (explicit) f.setRepo(undefined); else f.setWorktree(undefined); });
    await f.actions.handle(selected);
    assert.equal(f.calls.some(call => call[0] === 'mutation'), false, 'selection lost during worker startup');
  }
});

test('worktree removal reports nested workspace folders before confirmation or mutation', async () => {
  for (const selfRemoval of [false, true]) {
    const f = fixture();
    const removal = { ...f.getRepo(), path: selfRemoval ? f.root : path.resolve('workspace/linked'),
      repoIdentity: 'a'.repeat(64), selfRemoval };
    f.setRepo({ ...f.getRepo(), worktreeRemoval: removal });
    f.setWorktreePreview(async () => { throw Object.assign(new Error('Cannot remove a worktree containing another workspace folder.'), { code: 'invalid_request' }); });
    await f.actions.handle(action('removeWorkspaceWorktree', 'repo'));
    assert.match(f.errors.at(-1), /worktree containing another workspace folder/i);
    assert.equal(f.calls.some(call => call[0] === 'confirm' || call[0] === 'mutation'), false);
  }
});

test('ordinary repository menus do not offer worktree removal', async () => {
  const f = fixture();
  f.pick(choices => { assert.ok(!choices.some(choice => choice.action === 'removeWorkspaceWorktree')); return undefined; });
  await f.actions.showMenu('repo', 'repo');
  await f.actions.handle(action('removeWorkspaceWorktree', 'repo'));
  assert.deepEqual(f.calls, []);
});

test('delegated worktree repository actions stop when the worktree link disappears during selection', async () => {
  const f = fixture();
  f.setWorktree({ ...f.getWorktree(), available: true, childRepoKey: 'linked-repo', childRepoId: 'repo' });
  f.setRepo({ ...f.getRepo(), branch: 'topic', head: 'a'.repeat(40) });
  let menu = 0;
  f.pick(choices => {
    if (menu++ === 0) return choices.find(choice => choice.action === 'worktreeRepositoryActions');
    f.setWorktree(undefined);
    return choices.find(choice => choice.action === 'push');
  });
  await f.actions.showMenu('worktree', 'worktree');
  assert.deepEqual(f.calls, []);
});

test('delegated worktree mutation rechecks the worktree link after the worker handshake', async () => {
  const f = fixture();
  f.setWorktree({ ...f.getWorktree(), available: true, childRepoKey: 'linked-repo', childRepoId: 'repo' });
  f.setRepo({ ...f.getRepo(), branch: 'topic', head: 'a'.repeat(40) });
  let menu = 0;
  f.pick(choices => {
    const selected = menu++ === 0 ? 'worktreeRepositoryActions' : 'push';
    return choices.find(choice => choice.action === selected);
  });
  f.beforeValidation(() => f.setWorktree(undefined));
  await f.actions.showMenu('worktree', 'worktree');
  assert.equal(f.calls.some(call => call[0] === 'mutation'), false);
  assert.equal(f.calls.at(-1)[0], 'refresh');
  assert.match(f.errors[0], /stale/);
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
