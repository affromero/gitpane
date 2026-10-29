'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const childProcess = require('node:child_process');
const { spawnSync } = childProcess;
const { isolatedGitEnv } = require('./git-env');

const executable = process.env.GITPANE_TEST_BINARY;
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-extension-real-'));
const repoPath = path.join(temporary, 'workspace', 'repo');
const worktreePath = path.join(temporary, 'workspace', 'linked');
const oldConfig = process.env.GITPANE_CONFIG;
let context;

function uri(value, scheme = 'file') {
  return { scheme, fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = handler => { this.listeners.add(handler); return { dispose: () => this.listeners.delete(handler) }; }; }
  fire(value) { for (const handler of this.listeners) handler(value); }
  dispose() { this.listeners.clear(); }
}

const event = () => ({ dispose() {} });
const commands = new Map();
const documents = new Map();
const diffs = [];
const errors = [];
const warnings = [];
const snapshots = [];
const children = [];
const terminals = [];
let provider;
let receive;
let pickChanges;
let submoduleInput;
const workspaceState = new Map();

async function main() {
  if (!executable || !path.isAbsolute(executable) || !fs.existsSync(executable)) {
    throw new Error('Set GITPANE_TEST_BINARY to the absolute path of the built Gitpane service.');
  }
  fs.mkdirSync(repoPath, { recursive: true });
  const gitAt = (directory, ...args) => {
    const result = spawnSync('git', ['-C', directory, '-c', 'user.name=Gitpane Test',
      '-c', 'user.email=gitpane-test@example.invalid', ...args], { env: isolatedGitEnv(), encoding: 'utf8' });
    if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr);
    return result.stdout.trim();
  };
  const git = (...args) => gitAt(repoPath, ...args);
  git('init', '-q');
  git('commit', '--allow-empty', '-qm', 'initial');
  git('worktree', 'add', '-qb', 'feature/worktree', worktreePath);
  for (let index = 0; index < 130; index++) {
    fs.writeFileSync(path.join(repoPath, `file-${String(index).padStart(3, '0')}.txt`), `change ${index}\n`);
    fs.writeFileSync(path.join(worktreePath, `worktree-${String(index).padStart(3, '0')}.txt`), `worktree change ${index}\n`);
  }
  git('init', '-q', 'nested-repo');
  fs.writeFileSync(path.join(repoPath, 'nested-repo', 'nested.txt'), 'nested\n');
  process.env.GITPANE_CONFIG = path.join(temporary, 'no-config.toml');
  const repo = { rootUri: uri(repoPath), state: { HEAD: { name: git('branch', '--show-current'),
    commit: git('rev-parse', 'HEAD') }, onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [] },
  async status() {} };
  const worktree = { rootUri: uri(worktreePath), kind: 'worktree', state: { HEAD: {
    name: 'feature/worktree', commit: git('rev-parse', 'HEAD') }, onDidChange: event,
  workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  const openedRepositories = new EventEmitter();
  const closedRepositories = new EventEmitter();
  const workspaceChanged = new EventEmitter();
  const api = { repositories: [repo, worktree], onDidOpenRepository: openedRepositories.event, onDidCloseRepository: closedRepositories.event,
    toGitUri: (value, ref) => ({ scheme: 'git', fsPath: value.fsPath, ref }) };
  const view = { visible: true, webview: {
    postMessage: snapshot => snapshots.push(snapshot),
    onDidReceiveMessage: handler => { receive = handler; return { dispose() {} }; }
  }, onDidChangeVisibility: event, onDidDispose: event };
  const vscode = {
    EventEmitter, Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ProgressLocation: { Notification: 1 },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(path.dirname(repoPath)) }],
      getConfiguration: () => ({ get: name => name === 'sessionBackend' ? 'auto' : executable }), onDidChangeWorkspaceFolders: workspaceChanged.event,
      onDidChangeConfiguration: event,
      registerTextDocumentContentProvider: (scheme, handler) => { documents.set(scheme, handler); return event(); },
      fs: { stat: async value => fs.promises.stat(value.fsPath) } },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      createTerminal: options => { terminals.push(options); return { show() {}, dispose() {} }; },
      registerWebviewViewProvider: (_name, value) => { provider = value; return { dispose() {} }; },
      withProgress: async (_options, operation) => operation({ report() {} }),
      showQuickPick: async choices => pickChanges?.(choices),
      showInputBox: async () => submoduleInput,
      showWarningMessage: async (message, options, action) => {
        if (typeof options === 'string') { warnings.push(message); return undefined; }
        return action;
      },
      showInformationMessage: async () => {},
      showErrorMessage: async message => errors.push(message) },
    commands: { registerCommand: (name, handler) => { commands.set(name, handler); return { dispose() {} }; },
      async executeCommand(name, ...args) {
        if (commands.has(name)) return commands.get(name)(...args);
        if (name === 'vscode.diff') diffs.push(args);
      } }
  };
  const load = Module._load;
  let activate;
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === 'node:child_process') return { ...childProcess, spawn(...command) {
        const child = childProcess.spawn(...command);
        const closed = new Promise(resolve => child.once('close', resolve));
        children.push({ child, closed });
        return child;
      } };
      return load.call(this, name, ...args);
    };
    ({ activate } = require('../../src/extension'));
    context = { extensionPath: path.resolve(__dirname, '../..'), extensionUri: '/extension', subscriptions: [],
      workspaceState: { get: key => workspaceState.get(key), async update(key, value) { workspaceState.set(key, structuredClone(value)); } } };
    await activate(context);
  } finally { Module._load = load; }
  // The HTML provider is a VS Code boundary; exercise the message handler directly.
  try {
    Module._load = function(name, ...args) {
      if (name === './webview/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    provider.resolveWebviewView(view);
  } finally { Module._load = load; }
  await receive({ type: 'ready' });
  for (let attempt = 0; attempt < 100 && !provider.theme.value && !provider.theme.error; attempt++) {
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.equal(provider.theme.error, undefined, provider.theme.error);
  assert.equal(provider.theme.value?.name, 'default');
  assert.equal(provider.theme.value.values['repo-list-worktree-count'], 'rgb(255, 175, 0)');
  assert.ok(provider.theme.value.lanes.length > 0);
  const owner = snapshots.at(-1).repositories[0];
  assert.equal(repo.state.workingTreeChanges.length, 0);
  assert.equal(owner.dirty, true);
  await receive({ type: 'loadChanges', id: owner.id });
  const first = snapshots.at(-1).repositories[0];
  assert.equal(first.changesPage.next, true);
  assert.equal(first.groups.flatMap(group => group.files).length, 128);
  await receive({ type: 'changesPage', id: owner.id, direction: 'next' });
  const second = snapshots.at(-1).repositories[0];
  assert.equal(second.changesPage.previous, true);
  assert.equal(second.changesPage.next, false);
  const directory = second.groups.flatMap(group => group.files).find(item => item.path === 'nested-repo/');
  assert.ok(directory?.directory, 'embedded repository must appear as an informational directory');
  assert.equal(provider.changes.has(directory.id), false, 'directory must not resolve as an actionable file');
  await receive({ type: 'openChange', id: directory.id });
  assert.equal(diffs.length, 0);
  const file = second.groups.flatMap(group => group.files)[0];
  assert.ok(file);
  await receive({ type: 'openChange', id: file.id });
  assert.equal(diffs.length, 1);
  assert.equal(diffs[0][0].scheme, 'gitpane-empty');
  assert.equal(diffs[0][1].fsPath, path.join(repoPath, file.path));
  const stale = provider.changes.get(file.id);
  fs.writeFileSync(path.join(repoPath, 'unrelated.txt'), 'unrelated\n');
  await commands.get('gitpane.openChange')(stale);
  assert.equal(diffs.length, 2, 'an unrelated change must not invalidate the selected file');
  fs.unlinkSync(path.join(repoPath, file.path));
  await commands.get('gitpane.openChange')(stale);
  assert.equal(diffs.length, 2);
  assert.match(errors.at(-1), /changed|no longer/i);
  await receive({ type: 'changesPage', id: owner.id, direction: 'next' });
  assert.equal(snapshots.at(-1).repositories[0].changesPage.page, 1);
  fs.writeFileSync(path.join(repoPath, 'new-after-refresh.txt'), 'new\n');
  await commands.get('gitpane.refresh')();
  assert.equal(snapshots.at(-1).repositories[0].changesPage.page, 0, 'manual refresh must immediately refresh detailed changes');
  console.log('PASS hidden Git API changes, paged native diff, unrelated edits, stale selection, and manual refresh');

  if (process.platform !== 'win32') {
    const outside = path.join(temporary, 'outside-target');
    const link = path.join(repoPath, '000-link');
    fs.writeFileSync(outside, 'outside file contents');
    fs.symlinkSync(outside, link);
    await commands.get('gitpane.refresh')();
    const linkFile = snapshots.at(-1).repositories[0].groups.flatMap(group => group.files)
      .find(entry => entry.path === '000-link');
    assert.ok(linkFile, 'untracked symlink should appear as a change');
    await receive({ type: 'openChange', id: linkFile.id });
    const document = diffs.at(-1)[1];
    assert.equal(document.scheme, 'gitpane-link');
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(document), outside);
    await receive({ type: 'changesPage', id: owner.id, direction: 'next' });
    fs.unlinkSync(link);
    fs.symlinkSync('missing-target', link);
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(document), 'missing-target');
    console.log('PASS symlink diff shows link text and remains current after paging');
  } else console.log('SKIP symlink diff on Windows: creating symlinks requires host privileges');

  await commands.get('gitpane.refresh')();
  const parent = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  const linked = parent?.worktrees.find(item => item.name === 'linked');
  assert.ok(linked?.available, 'linked worktree inside workspace must be available');
  assert.equal(worktree.state.workingTreeChanges.length, 0);
  await receive({ type: 'loadChanges', id: linked.id });
  const firstWorktree = snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees
    .find(item => item.id === linked.id);
  assert.equal(firstWorktree.changesPage.next, true);
  assert.equal(firstWorktree.groups.flatMap(group => group.files).length, 128);
  await receive({ type: 'changesPage', id: linked.id, direction: 'next' });
  const secondWorktree = snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees
    .find(item => item.id === linked.id);
  assert.equal(secondWorktree.changesPage.previous, true);
  assert.equal(secondWorktree.changesPage.next, false);
  const linkedFile = secondWorktree.groups.flatMap(group => group.files)[0];
  assert.ok(linkedFile, 'second worktree page must contain changed files');
  const priorDiffCount = diffs.length;
  await receive({ type: 'openChange', id: linkedFile.id });
  assert.equal(diffs.length, priorDiffCount + 1);
  assert.equal(diffs.at(-1)[0].scheme, 'gitpane-empty');
  assert.equal(diffs.at(-1)[1].fsPath, path.join(worktreePath, linkedFile.path));
  console.log('PASS real linked worktree paging and native file diff');

  pickChanges = choices => choices.find(choice => choice.action === 'hideWorktree');
  await receive({ type: 'menu', kind: 'worktree', id: linked.id });
  assert.ok(!snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees.some(item => item.name === 'linked'));
  assert.ok(api.repositories.includes(worktree), 'hiding a worktree does not close it in VS Code');
  const parentStatusKey = repo.rootUri.toString();
  const originalParentStatus = provider.tree.backendStatuses.get(parentStatusKey);
  api.repositories = api.repositories.filter(item => item !== worktree);
  closedRepositories.fire(worktree);
  provider.tree.backendStatuses.set(parentStatusKey, { ...originalParentStatus,
    worktrees: originalParentStatus.worktrees.map(item => fs.realpathSync(item.path) === fs.realpathSync(worktreePath)
      ? { ...item, repo_identity: undefined } : item) });
  provider.tree.changed();
  assert.ok(!snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees.some(item => item.name === 'linked'));
  assert.ok(snapshots.at(-1).errors.some(message => /identity verification is pending/i.test(message)),
    'a hidden unopened worktree with no identity must be reported as pending');
  provider.tree.backendStatuses.set(parentStatusKey, originalParentStatus);
  api.repositories.push(worktree);
  openedRepositories.fire(worktree);
  await commands.get('gitpane.refresh')();
  const onlyVisible = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  pickChanges = choices => choices.find(choice => choice.action === 'hideRepository');
  await receive({ type: 'menu', kind: 'repo', id: onlyVisible.id });
  assert.equal(snapshots.at(-1).repositories.length, 0);
  assert.match(snapshots.at(-1).emptyMessage, /repositories are hidden/i);
  pickChanges = choices => choices.find(choice => choice.label === 'repo');
  await commands.get('gitpane.restoreHiddenRepositories')();
  pickChanges = choices => choices.find(choice => choice.label === 'linked');
  await commands.get('gitpane.restoreHiddenRepositories')();
  assert.ok(snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees.some(item => item.name === 'linked'));
  const anchor = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  pickChanges = choices => choices.find(choice => choice.action === 'hideRepository');
  await receive({ type: 'menu', kind: 'repo', id: anchor.id });
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'repo'));
  assert.ok(snapshots.at(-1).repositories.some(item => item.name === 'linked'), 'a hidden anchor must not hide its opened worktree');
  assert.ok(api.repositories.includes(repo), 'hiding an anchor does not close it in VS Code');
  const verifiedStatuses = new Map(provider.tree.backendStatuses);
  provider.tree.backendStatuses.clear();
  provider.tree.changed();
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'repo'), 'a hidden row stays hidden while status is unavailable');
  assert.ok(snapshots.at(-1).errors.some(message => /identity verification is pending/i.test(message)));
  provider.tree.backendStatuses.set(parentStatusKey, { ...verifiedStatuses.get(parentStatusKey), repo_identity: 'malformed' });
  provider.tree.changed();
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'repo'), 'malformed identity must not reveal a hidden row');
  assert.ok(snapshots.at(-1).errors.some(message => /identity verification is pending/i.test(message)));
  for (const [key, status] of verifiedStatuses) provider.tree.backendStatuses.set(key, status);
  provider.tree.changed();
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'repo'));
  const latePath = path.join(path.dirname(repoPath), 'late');
  fs.mkdirSync(latePath);
  gitAt(latePath, 'init', '-q');
  gitAt(latePath, 'commit', '--allow-empty', '-qm', 'initial');
  const lateRepo = { rootUri: uri(latePath), state: { HEAD: { commit: gitAt(latePath, 'rev-parse', 'HEAD') },
    onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  api.repositories.push(lateRepo);
  openedRepositories.fire(lateRepo);
  await commands.get('gitpane.refresh')();
  assert.ok(snapshots.at(-1).repositories.some(item => item.name === 'late'), 'new workspace repositories remain visible');
  api.repositories = api.repositories.filter(item => item !== lateRepo);
  closedRepositories.fire(lateRepo);
  pickChanges = choices => choices.find(choice => choice.label === 'repo');
  await commands.get('gitpane.restoreHiddenRepositories')();
  assert.ok(snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees.some(item => item.name === 'linked'));
  console.log('PASS hide and restore repository and worktree overview rows');

  const mainRemote = path.join(temporary, 'main-remote.git');
  const linkedRemote = path.join(temporary, 'linked-remote.git');
  const mainWriter = path.join(temporary, 'main-writer');
  const linkedWriter = path.join(temporary, 'linked-writer');
  for (const remote of [mainRemote, linkedRemote]) {
    git('init', '--bare', '-q', remote);
    gitAt(remote, 'symbolic-ref', 'HEAD', 'refs/heads/main');
  }
  git('remote', 'add', 'origin', mainRemote);
  git('remote', 'add', 'review', linkedRemote);
  git('push', '-q', 'origin', 'HEAD:refs/heads/main');
  git('push', '-q', 'review', 'HEAD:refs/heads/main');
  git('config', `branch.${git('branch', '--show-current')}.remote`, 'origin');
  git('config', 'branch.feature/worktree.remote', 'review');
  for (const [remote, writer] of [[mainRemote, mainWriter], [linkedRemote, linkedWriter]]) {
    git('clone', '-q', remote, writer);
    fs.writeFileSync(path.join(writer, 'remote-change.txt'), writer);
    gitAt(writer, 'add', 'remote-change.txt');
    gitAt(writer, 'commit', '-qm', 'remote change');
    gitAt(writer, 'push', '-q');
  }
  const oldMain = git('rev-parse', 'refs/remotes/origin/main');
  const oldLinked = git('rev-parse', 'refs/remotes/review/main');
  await commands.get('gitpane.refresh')();
  assert.equal(git('rev-parse', 'refs/remotes/origin/main'), oldMain, 'ordinary refresh must not fetch');
  assert.equal(git('rev-parse', 'refs/remotes/review/main'), oldLinked, 'ordinary refresh must not fetch linked worktree remote');
  const errorsBeforeFetch = errors.length;
  const warningsBeforeFetch = warnings.length;
  await commands.get('gitpane.fetchWorkspace')();
  assert.equal(errors.length, errorsBeforeFetch, errors.at(-1));
  assert.equal(warnings.length, warningsBeforeFetch, warnings.at(-1));
  assert.equal(git('rev-parse', 'refs/remotes/origin/main'), gitAt(mainWriter, 'rev-parse', 'HEAD'));
  assert.equal(git('rev-parse', 'refs/remotes/review/main'), gitAt(linkedWriter, 'rev-parse', 'HEAD'));
  console.log('PASS explicit workspace fetch respects different worktree remotes');

  vscode.workspace.workspaceFolders = [{ uri: uri(path.dirname(repoPath)) }, { uri: uri(worktreePath) }];
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  const explicit = snapshots.at(-1).repositories.find(item => item.name === 'linked');
  assert.ok(explicit, 'an explicitly added worktree must remain a top-level repository');
  let sawRemove = false;
  pickChanges = choices => {
    const selected = choices.find(choice => choice.action === 'removeWorkspaceWorktree');
    if (selected) sawRemove = true;
    return selected;
  };
  const previousErrors = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: explicit.id });
  assert.equal(sawRemove, true, 'worktree repository menu must offer removal');
  assert.equal(errors.length, previousErrors + 1);
  assert.match(errors.at(-1), /modified|dirty|untracked/i, 'dirty worktree must be refused');
  assert.ok(fs.existsSync(worktreePath));
  for (let index = 0; index < 130; index++) {
    fs.unlinkSync(path.join(worktreePath, `worktree-${String(index).padStart(3, '0')}.txt`));
  }
  await commands.get('gitpane.refresh')();
  const cleanExplicit = snapshots.at(-1).repositories.find(item => item.name === 'linked');
  await receive({ type: 'menu', kind: 'repo', id: cleanExplicit.id });
  assert.equal(fs.existsSync(worktreePath), false, errors.at(-1));
  assert.ok(!git('worktree', 'list', '--porcelain').includes(worktreePath));
  assert.equal(vscode.workspace.workspaceFolders.length, 2, 'the missing workspace folder stays configured');
  assert.ok(snapshots.at(-1).errors.some(message => message.includes(worktreePath) && /unavailable/i.test(message)));
  fs.writeFileSync(path.join(repoPath, '000-after-worktree-removal.txt'), 'remaining repository works\n');
  await commands.get('gitpane.refresh')();
  const remaining = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  assert.ok(remaining && !remaining.statusUnavailable, 'remaining repository status must still load');
  await receive({ type: 'loadChanges', id: remaining.id });
  const postRemovalFile = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
    .flatMap(group => group.files).find(file => file.path === '000-after-worktree-removal.txt');
  assert.ok(postRemovalFile);
  pickChanges = choices => choices.find(choice => choice.action === 'stage');
  const errorsBeforeStage = errors.length;
  await receive({ type: 'menu', kind: 'file', id: postRemovalFile.id });
  assert.equal(errors.length, errorsBeforeStage, errors.at(-1));
  assert.match(git('status', '--porcelain', '--', postRemovalFile.path), /^A /);
  console.log('PASS explicit worktree removal retains missing workspace folder and remaining repository actions');

  const source = path.join(temporary, 'submodule-source');
  fs.mkdirSync(source);
  gitAt(source, 'init', '-q');
  gitAt(source, 'commit', '--allow-empty', '-qm', 'submodule initial');
  git('-c', 'protocol.file.allow=always', 'submodule', 'add', '-q', source, 'child');
  git('commit', '-qm', 'add submodule');
  repo.state.HEAD.commit = git('rev-parse', 'HEAD');
  const childPath = fs.realpathSync(path.join(repoPath, 'child'));
  for (let index = 0; index < 130; index++) {
    fs.writeFileSync(path.join(childPath, `child-${String(index).padStart(3, '0')}.txt`), `child change ${index}\n`);
  }
  const childRepo = { rootUri: uri(childPath), state: { HEAD: { commit: gitAt(childPath, 'rev-parse', 'HEAD') },
    onDidChange: event, submodules: [], workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  const availableSubmodules = new Map([[childPath, childRepo]]);
  api.openRepository = async target => {
    const submodule = availableSubmodules.get(target.fsPath);
    if (!submodule) throw new Error(`Unexpected repository ${target.fsPath}`);
    if (!api.repositories.includes(submodule)) { api.repositories.push(submodule); openedRepositories.fire(submodule); }
    return submodule;
  };
  await commands.get('gitpane.refresh')();
  const parentAfterSubmodule = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  const submodule = parentAfterSubmodule.groups.flatMap(group => group.files).find(item => item.path === 'child');
  assert.ok(submodule, 'dirty in-workspace submodule must appear in the parent changes');
  assert.equal(childRepo.state.workingTreeChanges.length, 0, 'VS Code child changes are intentionally empty');
  let selectedChild;
  pickChanges = choices => {
    if (choices.some(choice => choice.kind === 'browse')) return choices.find(choice => choice.kind === 'browse');
    if (choices.some(choice => choice.kind === 'next')) return choices.find(choice => choice.kind === 'next');
    const choice = choices.find(item => item.kind === 'change');
    selectedChild = choice?.record?.path;
    return choice;
  };
  const priorSubmoduleDiffs = diffs.length;
  await receive({ type: 'openChange', id: submodule.id });
  assert.equal(diffs.length, priorSubmoduleDiffs + 1, errors.at(-1));
  assert.match(selectedChild, /^child-12[89]\.txt$/);
  assert.equal(diffs.at(-1)[0].scheme, 'gitpane-empty');
  assert.equal(diffs.at(-1)[1].fsPath, path.join(childPath, selectedChild));
  pickChanges = choices => {
    if (choices.some(choice => choice.kind === 'browse')) return choices.find(choice => choice.kind === 'browse');
    const choice = choices.find(item => item.kind === 'change');
    fs.unlinkSync(path.join(childPath, choice.record.path));
    return choice;
  };
  await receive({ type: 'openChange', id: submodule.id });
  assert.equal(diffs.length, priorSubmoduleDiffs + 1);
  assert.match(errors.at(-1), /changed|no longer/i);
  pickChanges = choices => {
    api.repositories = api.repositories.filter(item => item !== childRepo);
    closedRepositories.fire(childRepo);
    return choices.find(choice => choice.kind === 'graph');
  };
  await receive({ type: 'openChange', id: submodule.id });
  assert.equal(diffs.length, priorSubmoduleDiffs + 1);
  assert.match(errors.at(-1), /changed|no longer/i);
  console.log('PASS hidden submodule changes, second page native diff, and stale child guards');

  for (const name of fs.readdirSync(repoPath)) {
    if (/^file-\d{3}\.txt$/.test(name) || ['nested-repo', 'unrelated.txt', 'new-after-refresh.txt', '000-link'].includes(name)) {
      fs.rmSync(path.join(repoPath, name), { recursive: true, force: true });
    }
  }
  for (const name of fs.readdirSync(childPath)) {
    if (/^child-\d{3}\.txt$/.test(name)) fs.unlinkSync(path.join(childPath, name));
  }
  fs.writeFileSync(path.join(childPath, 'unpushed.txt'), 'committed locally\n');
  gitAt(childPath, 'add', 'unpushed.txt');
  gitAt(childPath, 'commit', '-qm', 'local child commit');
  childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
  git('add', 'child');
  git('commit', '-qm', 'record unpushed child');
  repo.state.HEAD.commit = git('rev-parse', 'HEAD');
  assert.equal(git('status', '--porcelain'), '', 'parent must be clean');
  await commands.get('gitpane.refresh')();
  const cleanParent = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  assert.equal(cleanParent.dirty, false);
  assert.equal(cleanParent.changeCountLowerBound, 0);
  assert.equal(cleanParent.hasUnpushedSubmodules, true);
  await receive({ type: 'loadChanges', id: cleanParent.id });
  const warnedParent = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  const warningGroup = warnedParent.groups.find(group => group.id === 'attention');
  assert.equal(warningGroup?.label, 'Submodules needing attention');
  assert.equal(warningGroup.files.length, 1);
  assert.equal(warningGroup.files[0].path, 'child');
  assert.equal(warningGroup.files[0].status, '!');
  let inspectorChoices;
  pickChanges = choices => { inspectorChoices = choices; return undefined; };
  const errorsBeforeWarning = errors.length;
  const diffsBeforeWarning = diffs.length;
  const terminalsBeforeWarning = terminals.length;
  await receive({ type: 'openChange', id: warningGroup.files[0].id });
  assert.equal(errors.length, errorsBeforeWarning, errors.at(-1));
  assert.ok(inspectorChoices?.some(choice => choice.kind === 'graph'), 'warning click must open the submodule inspector');
  assert.equal(diffs.length, diffsBeforeWarning);
  assert.equal(terminals.length, terminalsBeforeWarning);
  console.log('PASS clean parent unpushed submodule warning opens the scoped inspector without a terminal');

  const pointerA = git('rev-parse', 'HEAD:child');
  fs.writeFileSync(path.join(childPath, 'unpushed.txt'), 'staged child commit\n');
  gitAt(childPath, 'add', 'unpushed.txt');
  gitAt(childPath, 'commit', '-qm', 'staged child commit');
  const pointerB = gitAt(childPath, 'rev-parse', 'HEAD');
  git('add', 'child');
  fs.writeFileSync(path.join(childPath, 'unpushed.txt'), 'working child commit\n');
  gitAt(childPath, 'add', 'unpushed.txt');
  gitAt(childPath, 'commit', '-qm', 'working child commit');
  const pointerC = gitAt(childPath, 'rev-parse', 'HEAD');
  childRepo.state.HEAD.commit = pointerC;
  await commands.get('gitpane.refresh')();
  const pointerParent = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: pointerParent.id });
  const pointerGroups = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups;
  const stagedPointer = pointerGroups.find(group => group.id === 'index')?.files.find(item => item.path === 'child');
  const workingPointer = pointerGroups.find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(stagedPointer && workingPointer, 'staged and working gitlink changes must both be selectable');
  pickChanges = choices => choices.find(choice => choice.kind === 'pointer' && choice.file.path === 'unpushed.txt');
  const beforePointerDiffs = diffs.length;
  await receive({ type: 'openChange', id: stagedPointer.id });
  assert.equal(diffs.length, beforePointerDiffs + 1, errors.at(-1));
  assert.equal(diffs.at(-1)[0].ref, pointerA, 'staged diff starts at the committed parent pointer');
  assert.equal(diffs.at(-1)[1].ref, pointerB, 'staged diff ends at the index pointer');
  await receive({ type: 'openChange', id: workingPointer.id });
  assert.equal(diffs.length, beforePointerDiffs + 2, errors.at(-1));
  assert.equal(diffs.at(-1)[0].ref, pointerB, 'working diff starts at the index pointer');
  assert.equal(diffs.at(-1)[1].ref, pointerC, 'working diff ends at the live child HEAD');
  console.log('PASS staged and unstaged submodule file clicks use their selected A→B and B→C native diffs');

  git('commit', '-qm', 'record staged pointer');
  const pointerCommit = git('rev-parse', 'HEAD');
  repo.state.HEAD.commit = pointerCommit;
  await commands.get('gitpane.refresh')();
  let graphReceive, graphDispose;
  const graphMessages = [];
  const graphPanel = { visible: true, webview: { postMessage: message => graphMessages.push(message),
    onDidReceiveMessage: handler => { graphReceive = handler; return event(); } },
    onDidDispose: handler => { graphDispose = handler; return event(); }, onDidChangeViewState: event, reveal() {}, dispose() {} };
  vscode.ViewColumn = { Active: 1 };
  vscode.window.createWebviewPanel = () => graphPanel;
  const graphRoot = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  try {
    Module._load = function(name, ...args) {
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    await commands.get('gitpane.openGraph')(graphRoot.id);
  } finally { Module._load = load; }
  await graphReceive({ type: 'ready' });
  await graphReceive({ type: 'selectCommit', oid: pointerCommit });
  const committedPointer = graphMessages.at(-1).files.find(file => file.path === 'child');
  assert.ok(committedPointer, 'committed gitlink must be listed in the graph');
  const beforeGraphDiff = diffs.length, beforeGraphTerminals = terminals.length, beforeGraphRepos = api.repositories.length;
  await graphReceive({ type: 'openCommitFile', id: committedPointer.id });
  assert.equal(diffs.length, beforeGraphDiff + 1, errors.at(-1));
  const pointerDocuments = [diffs.at(-1)[0], diffs.at(-1)[1]];
  await graphReceive({ type: 'selectCommit', oid: git('rev-parse', 'HEAD^') });
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointerDocuments[0]), `Subproject commit ${pointerA}\n`);
  graphDispose();
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointerDocuments[1]), `Subproject commit ${pointerB}\n`);
  assert.equal(terminals.length, beforeGraphTerminals);
  assert.equal(api.repositories.length, beforeGraphRepos, 'opening a pointer diff does not open a child repository');
  console.log('PASS committed submodule pointer opens a native diff without a child checkout or terminal');

  const grandSource = path.join(temporary, 'grandchild-source');
  fs.mkdirSync(grandSource);
  gitAt(grandSource, 'init', '-q');
  fs.writeFileSync(path.join(grandSource, 'nested.txt'), 'original\n');
  gitAt(grandSource, 'add', 'nested.txt');
  gitAt(grandSource, 'commit', '-qm', 'nested initial');
  gitAt(childPath, '-c', 'protocol.file.allow=always', 'submodule', 'add', '-q', grandSource, 'nested');
  gitAt(childPath, 'commit', '-qm', 'add nested submodule');
  childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
  const grandPath = fs.realpathSync(path.join(childPath, 'nested'));
  const grandRepo = { rootUri: uri(grandPath), state: { HEAD: { commit: gitAt(grandPath, 'rev-parse', 'HEAD') },
    onDidChange: event, submodules: [], workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  availableSubmodules.set(grandPath, grandRepo);
  childRepo.state.submodules = [{ path: 'nested', url: grandSource }];
  if (!api.repositories.includes(childRepo)) { api.repositories.push(childRepo); openedRepositories.fire(childRepo); }
  await commands.get('gitpane.refresh')();
  const browseRoot = snapshots.at(-1).repositories.find(item => item.name === 'child');
  assert.ok(browseRoot, 'the already-open parent must be visible');
  let steps = ['browseSubmodules'];
  const beforeBrowseRepos = api.repositories.length;
  pickChanges = choices => {
    const next = steps.shift();
    if (!next) return undefined;
    return choices.find(choice => choice.action === next || choice.relative === next);
  };
  const beforeBrowseErrors = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: browseRoot.id });
  assert.equal(errors.length, beforeBrowseErrors, errors.at(-1));
  assert.equal(api.repositories.length, beforeBrowseRepos, 'browsing and cancellation do not open repositories');
  steps = ['browseSubmodules', 'manual'];
  submoduleInput = 'missing-submodule';
  pickChanges = choices => {
    const next = steps.shift();
    return choices.find(choice => choice.action === next || (next === 'manual' && choice.manual));
  };
  await receive({ type: 'menu', kind: 'repo', id: browseRoot.id });
  assert.equal(errors.length, beforeBrowseErrors + 1, 'invalid submodule paths must report an error');
  assert.equal(provider.tree.backend.state.status, 'ready', 'an invalid selection must not mark the backend unavailable');
  assert.ok(!snapshots.at(-1).repositories.find(item => item.name === 'child').statusUnavailable);
  steps = ['browseSubmodules', 'nested'];
  pickChanges = choices => {
    const next = steps.shift();
    return choices.find(choice => choice.action === next || choice.relative === next);
  };
  await receive({ type: 'menu', kind: 'repo', id: browseRoot.id });
  assert.equal(errors.length, beforeBrowseErrors + 1, errors.at(-1));
  assert.deepEqual(workspaceState.get('gitpane.submodulePins').pins[0].chain.map(link => link.path), ['nested']);
  assert.ok(api.repositories.includes(grandRepo), 'pinning opens the selected clean nested submodule');
  pickChanges = choices => choices[0];
  await commands.get('gitpane.manageSubmodulePins')();
  assert.equal(workspaceState.get('gitpane.submodulePins').pins.length, 0);
  console.log('PASS browse and pin clean nested submodules without opening on cancellation');
  fs.writeFileSync(path.join(grandPath, 'nested.txt'), 'working nested change\n');
  await commands.get('gitpane.refresh')();
  const nestedParent = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: nestedParent.id });
  const nestedWorking = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
    .find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(nestedWorking, 'parent must expose its changed child');
  pickChanges = choices => choices.find(choice => choice.kind === 'browse') ||
    choices.find(choice => choice.record?.path === 'nested') || choices.find(choice => choice.record?.path === 'nested.txt');
  const beforeNestedDiffs = diffs.length;
  const beforeNestedTerminals = terminals.length;
  await receive({ type: 'openChange', id: nestedWorking.id });
  assert.equal(diffs.length, beforeNestedDiffs + 1, errors.at(-1));
  assert.equal(diffs.at(-1)[1].fsPath, path.join(grandPath, 'nested.txt'));
  assert.equal(terminals.length, beforeNestedTerminals);
  let browseDepth = 0;
  pickChanges = choices => {
    if (choices.some(choice => choice.kind === 'browse')) {
      if (browseDepth++ === 0) return choices.find(choice => choice.kind === 'browse');
      return choices.find(choice => choice.kind === 'pin');
    }
    return choices.find(choice => choice.record?.path === 'nested');
  };
  const errorsBeforePin = errors.length;
  await receive({ type: 'openChange', id: nestedWorking.id });
  assert.equal(errors.length, errorsBeforePin, errors.at(-1));
  const pinState = workspaceState.get('gitpane.submodulePins');
  assert.equal(pinState.version, 1);
  assert.deepEqual(pinState.pins[0].chain.map(link => link.path), ['child', 'nested']);
  assert.ok(pinState.pins[0].chain.every(link => Object.keys(link).sort().join(',') === 'child_identity,parent_identity,path'));
  assert.equal(diffs.length, beforeNestedDiffs + 1, 'pinning must not open a terminal or diff');
  assert.equal(terminals.length, beforeNestedTerminals);
  const pinnedRow = snapshots.at(-1).repositories.find(item => item.name === 'nested');
  assert.ok(pinnedRow?.pinned, 'nested pin is visible in the overview');
  pickChanges = choices => choices.find(choice => choice.action === 'hideRepository');
  await receive({ type: 'menu', kind: 'repo', id: pinnedRow.id });
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'nested'));
  assert.equal(workspaceState.get('gitpane.submodulePins').pins.length, 1, 'hiding does not remove the pin');
  pickChanges = choices => choices.find(choice => choice.label === 'nested');
  await commands.get('gitpane.restoreHiddenRepositories')();
  const restoredPin = snapshots.at(-1).repositories.find(item => item.name === 'nested');
  assert.ok(restoredPin?.pinned, 'restoring retains the pin');
  pickChanges = choices => choices.find(choice => choice.action === 'unpinSubmodule');
  await receive({ type: 'menu', kind: 'repo', id: restoredPin.id });
  assert.deepEqual(workspaceState.get('gitpane.submodulePins').pins, []);
  assert.ok(api.repositories.includes(grandRepo), 'unpin does not close a shared VS Code repository');
  assert.ok(snapshots.at(-1).repositories.some(item => item.name === 'nested' && !item.pinned), 'the discovered repository remains visible without its pin');
  let changedAncestor = false;
  pickChanges = choices => {
    const selected = choices.find(choice => choice.record?.path === 'nested.txt');
    if (selected && !changedAncestor) {
      changedAncestor = true;
      gitAt(childPath, 'commit', '--allow-empty', '-qm', 'move ancestor while selecting');
      childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    }
    return choices.find(choice => choice.kind === 'browse') ||
      choices.find(choice => choice.record?.path === 'nested') || selected;
  };
  await receive({ type: 'openChange', id: nestedWorking.id });
  assert.equal(diffs.length, beforeNestedDiffs + 1, 'stale ancestor cannot open a nested diff');
  assert.match(errors.at(-1), /changed|no longer/i);
  console.log('PASS nested working submodule file opens a native diff and rejects changed ancestors');
}

main().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; }).finally(async () => {
  for (const disposable of context?.subscriptions || []) disposable.dispose();
  for (const entry of children) {
    let timer;
    try {
      await Promise.race([entry.closed, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('Gitpane service did not exit after disposal.')), 5000);
      })]);
    } catch (error) {
      entry.child.kill('SIGKILL');
      console.error(error.message);
      process.exitCode = 1;
    } finally { clearTimeout(timer); }
  }
  if (oldConfig === undefined) delete process.env.GITPANE_CONFIG;
  else process.env.GITPANE_CONFIG = oldConfig;
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});
