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
const snapshots = [];
const children = [];
const terminals = [];
let provider;
let receive;
let pickChanges;

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
  const api = { repositories: [repo, worktree], onDidOpenRepository: openedRepositories.event, onDidCloseRepository: closedRepositories.event,
    toGitUri: (value, ref) => ({ scheme: 'git', fsPath: value.fsPath, ref }) };
  const view = { visible: true, webview: {
    postMessage: snapshot => snapshots.push(snapshot),
    onDidReceiveMessage: handler => { receive = handler; return { dispose() {} }; }
  }, onDidChangeVisibility: event, onDidDispose: event };
  const vscode = {
    EventEmitter, Uri: { file: uri, joinPath: (...parts) => parts.join('/') },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(path.dirname(repoPath)) }],
      getConfiguration: () => ({ get: () => executable }), onDidChangeWorkspaceFolders: event,
      onDidChangeConfiguration: event,
      registerTextDocumentContentProvider: (scheme, handler) => { documents.set(scheme, handler); return event(); },
      fs: { stat: async value => fs.promises.stat(value.fsPath) } },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      createTerminal: options => { terminals.push(options); return { show() {}, dispose() {} }; },
      registerWebviewViewProvider: (_name, value) => { provider = value; return { dispose() {} }; },
      showQuickPick: async choices => pickChanges?.(choices),
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
    context = { extensionPath: path.resolve(__dirname, '../..'), extensionUri: '/extension', subscriptions: [] };
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
  api.openRepository = async target => {
    if (target.fsPath !== childPath) throw new Error(`Unexpected repository ${target.fsPath}`);
    if (!api.repositories.includes(childRepo)) { api.repositories.push(childRepo); openedRepositories.fire(childRepo); }
    return childRepo;
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
