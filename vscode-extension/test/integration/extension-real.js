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
const copied = [];
const errors = [];
const warnings = [];
const snapshots = [];
const children = [];
const terminals = [];
let provider;
let receive;
let pickChanges;
let submoduleInput;
let selectedWorkspaceRepository;
let confirmAction = true;
let onConfirm = () => {};
const confirmations = [];
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
  const gitBuffer = (directory, ref, relative) => {
    const result = spawnSync('git', ['-C', directory, 'show', '--textconv', `${ref}:${relative}`], { env: isolatedGitEnv() });
    if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr.toString());
    return result.stdout;
  };
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
  async status() {}, async buffer(ref, relative) { return gitBuffer(repoPath, ref, relative); } };
  const worktree = { rootUri: uri(worktreePath), kind: 'worktree', state: { HEAD: {
    name: 'feature/worktree', commit: git('rev-parse', 'HEAD') }, onDidChange: event,
  workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {},
  async buffer(ref, relative) { return gitBuffer(worktreePath, ref, relative); } };
  const openedRepositories = new EventEmitter();
  const closedRepositories = new EventEmitter();
  const workspaceChanged = new EventEmitter();
  const gitLookup = spawnSync(process.platform === 'win32' ? 'where' : 'which', ['git'], { encoding: 'utf8' });
  if (gitLookup.error || gitLookup.status !== 0) throw new Error('Cannot locate the Git executable for the integration test.');
  const api = { git: { path: gitLookup.stdout.trim().split(/\r?\n/)[0], env: isolatedGitEnv() },
    repositories: [repo, worktree], onDidOpenRepository: openedRepositories.event, onDidCloseRepository: closedRepositories.event,
    toGitUri: (value, ref) => ({ scheme: 'git', fsPath: value.fsPath, ref }) };
  const view = { visible: true, webview: {
    postMessage: snapshot => snapshots.push(snapshot),
    onDidReceiveMessage: handler => { receive = handler; return { dispose() {} }; }
  }, onDidChangeVisibility: event, onDidDispose: event };
  const vscode = {
    EventEmitter, Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ProgressLocation: { Notification: 1 },
    FileType: { File: 1 }, FileChangeType: { Changed: 1 },
    FileSystemError: { FileNotFound: () => Object.assign(new Error('File not found'), { code: 'FileNotFound' }) },
    env: { clipboard: { async writeText(value) { copied.push(value); } } },
    extensions: { getExtension: () => ({ activate: async () => ({ enabled: true, getAPI: () => api }) }) },
    workspace: { workspaceFolders: [{ uri: uri(path.dirname(repoPath)) }],
      getConfiguration: () => ({ get: name => name === 'sessionBackend' ? 'auto' : executable }), onDidChangeWorkspaceFolders: workspaceChanged.event,
      onDidChangeConfiguration: event,
      registerTextDocumentContentProvider: (scheme, handler) => { documents.set(scheme, handler); return event(); },
      registerFileSystemProvider: (scheme, handler) => { documents.set(scheme, handler); return event(); },
      fs: { stat: async value => fs.promises.stat(value.fsPath) } },
    window: { createOutputChannel: () => ({ append() {}, appendLine() {}, dispose() {} }),
      createTerminal: options => { terminals.push(options); return { show() {}, dispose() {} }; },
      registerWebviewViewProvider: (_name, value) => { provider = value; return { dispose() {} }; },
      withProgress: async (_options, operation) => operation({ report() {} }),
      showQuickPick: async choices => pickChanges?.(choices),
      showOpenDialog: async () => selectedWorkspaceRepository,
      showInputBox: async () => submoduleInput,
      showWarningMessage: async (message, options, action) => {
        if (typeof options === 'string') { warnings.push(message); return undefined; }
        confirmations.push(message);
        await onConfirm();
        return confirmAction ? action : undefined;
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
  const nestedPath = fs.realpathSync(path.join(repoPath, 'nested-repo'));
  const explicitlyAdded = { rootUri: uri(nestedPath), state: { HEAD: {}, onDidChange: event,
    workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  api.openRepository = async target => {
    if (target.fsPath !== nestedPath) throw new Error('Unexpected repository');
    if (!api.repositories.includes(explicitlyAdded)) { api.repositories.push(explicitlyAdded); openedRepositories.fire(explicitlyAdded); }
    return explicitlyAdded;
  };
  selectedWorkspaceRepository = [uri(nestedPath)];
  const errorsBeforeAdd = errors.length;
  await commands.get('gitpane.addWorkspaceRepository')();
  assert.equal(errors.length, errorsBeforeAdd, errors.at(-1));
  assert.ok(api.repositories.includes(explicitlyAdded));
  assert.equal(workspaceState.get('gitpane.workspaceRepositories').repositories[0].path, nestedPath);
  pickChanges = choices => choices.find(choice => choice.label === 'nested-repo');
  await commands.get('gitpane.manageWorkspaceRepositories')();
  assert.equal(workspaceState.get('gitpane.workspaceRepositories').repositories.length, 0);
  assert.ok(api.repositories.includes(explicitlyAdded), 'removing the saved addition must not close the repository');
  api.repositories = api.repositories.filter(item => item !== explicitlyAdded);
  closedRepositories.fire(explicitlyAdded);
  await commands.get('gitpane.refresh')();
  console.log('PASS explicit workspace repository addition and removal without closing VS Code repositories');
  const scopeRoot = path.join(temporary, 'additional-root');
  const movedScopeRoot = path.join(temporary, 'moved-additional-root');
  fs.mkdirSync(scopeRoot);
  const originalFolders = vscode.workspace.workspaceFolders;
  vscode.workspace.workspaceFolders = [...originalFolders, { uri: uri(scopeRoot) }];
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  fs.renameSync(scopeRoot, movedScopeRoot);
  await commands.get('gitpane.refresh')();
  assert.ok(!provider.tree.backend.roots.includes(scopeRoot));
  fs.renameSync(movedScopeRoot, scopeRoot);
  const scopeOwner = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  pickChanges = choices => choices.find(choice => choice.action === 'refreshRepo');
  const errorsBeforeScopeRefresh = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: scopeOwner.id });
  assert.equal(errors.length, errorsBeforeScopeRefresh, errors.at(-1));
  assert.ok(provider.tree.backend.roots.includes(fs.realpathSync(scopeRoot)), 'targeted refresh restores the original workspace identity');
  vscode.workspace.workspaceFolders = originalFolders;
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  console.log('PASS targeted refresh restores temporarily unavailable workspace scope');
  const blockedCases = process.platform === 'win32' ? [] : [['blocked-working', false, false, false], ['blocked-staged', true, false, false],
    ['blocked-link', false, true, false], ['blocked-conflict', false, false, true]];
  for (const [name, staged, symlink, conflict] of blockedCases) {
    const root = path.join(temporary, 'workspace', name), blocker = path.join(root, 'blocked');
    fs.mkdirSync(blocker, { recursive: true });
    const relative = 'blocked/:(glob)*.bin';
    const bytes = Buffer.from([0, 255, 42, 10]);
    fs.writeFileSync(path.join(root, relative), bytes);
    gitAt(root, 'init', '-q');
    gitAt(root, 'add', '-A');
    gitAt(root, 'commit', '-qm', 'tracked binary');
    fs.rmSync(blocker, { recursive: true });
    if (symlink) {
      const outside = path.join(temporary, 'outside-blocked');
      fs.mkdirSync(outside, { recursive: true });
      fs.writeFileSync(path.join(outside, ':(glob)*.bin'), 'outside');
      fs.symlinkSync(outside, blocker, 'dir');
    } else fs.writeFileSync(blocker, 'replacement');
    if (staged) gitAt(root, 'add', '-A');
    const ours = Buffer.from([0, 253, 1]), theirs = Buffer.from([0, 254, 2]);
    if (conflict) {
      const hash = data => {
        const result = spawnSync('git', ['-C', root, 'hash-object', '-w', '--stdin'], { env: isolatedGitEnv(), input: data });
        if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr.toString());
        return result.stdout.toString().trim();
      };
      const base = gitAt(root, 'rev-parse', `HEAD:${relative}`);
      const records = `0 ${'0'.repeat(40)}\t${relative}\n100644 ${base} 1\t${relative}\n` +
        `100644 ${hash(ours)} 2\t${relative}\n100644 ${hash(theirs)} 3\t${relative}\n`;
      const result = spawnSync('git', ['-C', root, 'update-index', '--index-info'], { env: isolatedGitEnv(), input: records });
      if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr.toString());
    }
    const added = { rootUri: uri(root), state: { HEAD: { commit: gitAt(root, 'rev-parse', 'HEAD'), name: 'main' },
      onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [], submodules: [] },
      async status() {}, async buffer(ref, file) { return gitBuffer(root, ref, file); } };
    api.repositories.push(added);
    openedRepositories.fire(added);
    await commands.get('gitpane.refresh')();
    let row = snapshots.at(-1).repositories.find(item => item.name === name);
    assert.ok(row, `repository ${name} must appear`);
    await receive({ type: 'loadChanges', id: row.id });
    row = snapshots.at(-1).repositories.find(item => item.name === name);
    const group = row.groups.find(item => item.id === (conflict ? 'merge' : staged ? 'index' : 'working'));
    const deleted = group?.files.find(item => item.path === relative);
    assert.ok(deleted, `deleted Git path must appear in ${name}`);
    const previousDiffs = diffs.length, previousErrors = errors.length;
    await receive({ type: 'openChange', id: deleted.id });
    assert.equal(errors.length, previousErrors, errors.at(-1));
    assert.equal(diffs.length, previousDiffs + 1);
    const [left, right] = diffs.at(-1);
    assert.equal(left.scheme, 'gitpane-blob');
    assert.equal(right.scheme, conflict ? 'gitpane-blob' : 'gitpane-empty');
    const files = documents.get('gitpane-blob');
    assert.deepEqual(await files.readFile(left), conflict ? ours : bytes);
    if (conflict) assert.deepEqual(await files.readFile(right), theirs);
    await receive({ type: 'releaseChanges', id: row.id });
    assert.deepEqual(await files.readFile(left), conflict ? ours : bytes, 'an open diff survives collapsing its source page');
    if (!staged && !symlink && !conflict) {
      const updated = Buffer.from([0, 252, 7]);
      const hashed = spawnSync('git', ['-C', root, 'hash-object', '-w', '--stdin'], { env: isolatedGitEnv(), input: updated });
      if (hashed.error || hashed.status !== 0) throw new Error(hashed.error?.message || hashed.stderr.toString());
      gitAt(root, 'update-index', '--add', '--cacheinfo', `100644,${hashed.stdout.toString().trim()},${relative}`);
      await files.refresh();
      assert.deepEqual(await files.readFile(left), updated, 'blocked Git sides follow index updates');
      gitAt(root, 'add', '-A');
      await assert.rejects(files.readFile(left), error => error.code === 'FileNotFound');
    }
    api.repositories.splice(api.repositories.indexOf(added), 1);
    closedRepositories.fire(added);
    await assert.rejects(files.readFile(left), /no longer/i);
  }
  if (blockedCases.length) console.log('PASS blocked historical Git sides use the owning repository for binary files');
  const overlapRoot = path.join(temporary, 'workspace', 'overlap-owner');
  const overlapChild = path.join(overlapRoot, 'nested');
  fs.mkdirSync(overlapChild, { recursive: true });
  fs.writeFileSync(path.join(overlapChild, 'tracked.txt'), 'parent history\n');
  gitAt(overlapRoot, 'init', '-q');
  gitAt(overlapRoot, 'add', '-A');
  gitAt(overlapRoot, 'commit', '-qm', 'parent history');
  gitAt(overlapChild, 'init', '-q');
  fs.writeFileSync(path.join(overlapChild, 'tracked.txt'), 'nested history\n');
  gitAt(overlapChild, 'add', '-A');
  gitAt(overlapChild, 'commit', '-qm', 'nested history');
  fs.writeFileSync(path.join(overlapChild, 'tracked.txt'), 'working change\n');
  const overlapRepo = { rootUri: uri(overlapRoot), state: { HEAD: { commit: gitAt(overlapRoot, 'rev-parse', 'HEAD'), name: 'main' },
    onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [], submodules: [] },
    async status() {}, async buffer(ref, file) { return gitBuffer(overlapRoot, ref, file); } };
  const overlapping = { rootUri: uri(overlapChild), state: { HEAD: { commit: gitAt(overlapChild, 'rev-parse', 'HEAD'), name: 'main' },
    onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [], submodules: [] },
    async status() {}, async buffer(ref, file) { return gitBuffer(overlapChild, ref, file); } };
  api.repositories.push(overlapRepo);
  openedRepositories.fire(overlapRepo);
  await commands.get('gitpane.refresh')();
  let overlapRow = snapshots.at(-1).repositories.find(item => item.name === 'overlap-owner');
  await receive({ type: 'loadChanges', id: overlapRow.id });
  overlapRow = snapshots.at(-1).repositories.find(item => item.name === 'overlap-owner');
  const overlapFile = overlapRow.groups.flatMap(group => group.files).find(item => item.path === 'nested/tracked.txt');
  assert.ok(overlapFile, 'parent repository must expose its tracked file');
  await receive({ type: 'openChange', id: overlapFile.id });
  const ownerSide = diffs.at(-1)[0], blobDocuments = documents.get('gitpane-blob');
  assert.equal((await blobDocuments.readFile(ownerSide)).toString(), 'parent history\n');
  api.repositories.push(overlapping);
  openedRepositories.fire(overlapping);
  assert.equal((await blobDocuments.readFile(ownerSide)).toString(), 'parent history\n', 'opening a nested repo cannot retarget an existing diff');
  gitAt(overlapRoot, 'add', 'nested/tracked.txt');
  fs.writeFileSync(path.join(overlapChild, 'tracked.txt'), 'new working change\n');
  await commands.get('gitpane.refresh')();
  await blobDocuments.refresh();
  assert.equal((await blobDocuments.readFile(ownerSide)).toString(), 'working change\n', 'index sides update from their selected owner');
  await receive({ type: 'releaseChanges', id: overlapRow.id });
  assert.equal((await blobDocuments.readFile(ownerSide)).toString(), 'working change\n', 'paging away does not close the diff');
  for (const item of [overlapping, overlapRepo]) { api.repositories.splice(api.repositories.indexOf(item), 1); closedRepositories.fire(item); }
  await assert.rejects(blobDocuments.readFile(ownerSide), /no longer/i);
  console.log('PASS live Git sides stay bound to their selected owner across nested repository opens and staging');
  if (process.platform !== 'win32') {
    const aliasRoot = path.join(temporary, 'workspace', 'index-alias');
    fs.mkdirSync(aliasRoot, { recursive: true });
    fs.writeFileSync(path.join(aliasRoot, '2:file'), 'stage zero\n');
    fs.writeFileSync(path.join(aliasRoot, 'file'), 'base\n');
    gitAt(aliasRoot, 'init', '-q');
    gitAt(aliasRoot, 'add', '-A');
    gitAt(aliasRoot, 'commit', '-qm', 'index alias');
    fs.writeFileSync(path.join(aliasRoot, '2:file'), 'working edit\n');
    const base = gitAt(aliasRoot, 'rev-parse', 'HEAD:file');
    const conflict = spawnSync('git', ['-C', aliasRoot, 'hash-object', '-w', '--stdin'],
      { env: isolatedGitEnv(), input: 'wrong conflict stage\n' });
    if (conflict.error || conflict.status !== 0) throw new Error(conflict.error?.message || conflict.stderr.toString());
    const records = `0 ${'0'.repeat(40)}\tfile\n100644 ${base} 1\tfile\n` +
      `100644 ${conflict.stdout.toString().trim()} 2\tfile\n100644 ${base} 3\tfile\n`;
    const changedIndex = spawnSync('git', ['-C', aliasRoot, 'update-index', '--index-info'], { env: isolatedGitEnv(), input: records });
    if (changedIndex.error || changedIndex.status !== 0) throw new Error(changedIndex.error?.message || changedIndex.stderr.toString());
    const aliasRepo = { rootUri: uri(aliasRoot), state: { HEAD: { commit: gitAt(aliasRoot, 'rev-parse', 'HEAD'), name: 'main' },
      onDidChange: event, workingTreeChanges: [], indexChanges: [], mergeChanges: [], submodules: [] },
      async status() {}, async buffer(ref, file) { return gitBuffer(aliasRoot, ref, file); } };
    api.repositories.push(aliasRepo);
    openedRepositories.fire(aliasRepo);
    await commands.get('gitpane.refresh')();
    let aliasRow = snapshots.at(-1).repositories.find(item => item.name === 'index-alias');
    await receive({ type: 'loadChanges', id: aliasRow.id });
    aliasRow = snapshots.at(-1).repositories.find(item => item.name === 'index-alias');
    const aliasFile = aliasRow.groups.flatMap(group => group.files).find(item => item.path === '2:file');
    assert.ok(aliasFile, 'working change for the stage-zero colon filename must appear');
    const priorDiffs = diffs.length, priorErrors = errors.length;
    await receive({ type: 'openChange', id: aliasFile.id });
    assert.equal(errors.length, priorErrors, errors.at(-1));
    assert.equal(diffs.length, priorDiffs + 1);
    assert.equal((await blobDocuments.readFile(diffs.at(-1)[0])).toString(), 'stage zero\n');
    api.repositories.splice(api.repositories.indexOf(aliasRepo), 1);
    closedRepositories.fire(aliasRepo);
    console.log('PASS index stage-zero colon filenames cannot alias conflict stages');
  }
  if (process.platform !== 'win32') {
    const convertRoot = path.join(temporary, 'workspace', 'textconv-repo');
    const converterOutput = path.join(temporary, 'converter-output');
    fs.mkdirSync(convertRoot, { recursive: true });
    fs.writeFileSync(path.join(convertRoot, '.gitattributes'), '*.txt diff=gitpane-test\n');
    fs.writeFileSync(path.join(convertRoot, 'file.txt'), 'original\n');
    fs.writeFileSync(converterOutput, 'converted one\n');
    gitAt(convertRoot, 'init', '-q');
    const quoted = `'${converterOutput.replace(/'/g, "'\\''")}'`;
    const converterScript = path.join(temporary, 'gitpane-converter.sh');
    fs.writeFileSync(converterScript, `#!/bin/sh\ncat ${quoted}\n`, { mode: 0o755 });
    gitAt(convertRoot, 'config', 'diff.gitpane-test.textconv', `'${converterScript.replace(/'/g, "'\\''")}'`);
    gitAt(convertRoot, 'add', '-A');
    gitAt(convertRoot, 'commit', '-qm', 'textconv');
    fs.writeFileSync(path.join(convertRoot, 'file.txt'), 'changed\n');
    const changed = new EventEmitter();
    const convertRepo = { rootUri: uri(convertRoot), state: { HEAD: { commit: gitAt(convertRoot, 'rev-parse', 'HEAD'), name: 'main' },
      onDidChange: changed.event, workingTreeChanges: [], indexChanges: [], mergeChanges: [], submodules: [] },
      async status() {}, async buffer(ref, file) { return gitBuffer(convertRoot, ref, file); } };
    api.repositories.push(convertRepo);
    openedRepositories.fire(convertRepo);
    await commands.get('gitpane.refresh')();
    let convertRow = snapshots.at(-1).repositories.find(item => item.name === 'textconv-repo');
    await receive({ type: 'loadChanges', id: convertRow.id });
    convertRow = snapshots.at(-1).repositories.find(item => item.name === 'textconv-repo');
    const convertFile = convertRow.groups.flatMap(group => group.files).find(item => item.path === 'file.txt');
    const priorDiffs = diffs.length, priorErrors = errors.length;
    await receive({ type: 'openChange', id: convertFile.id });
    assert.equal(errors.length, priorErrors, errors.at(-1));
    assert.equal(diffs.length, priorDiffs + 1);
    const convertSide = diffs.at(-1)[0], before = await blobDocuments.stat(convertSide), changeEvents = [];
    const subscription = blobDocuments.onDidChangeFile(events => changeEvents.push(...events));
    assert.equal((await blobDocuments.readFile(convertSide)).toString(), 'converted one\n');
    fs.writeFileSync(converterOutput, 'converted two\n');
    changed.fire();
    await blobDocuments.refresh();
    const refreshDeadline = Date.now() + 5000;
    while (!changeEvents.some(item => item.uri.toString() === convertSide.toString()) && Date.now() < refreshDeadline) {
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.ok(changeEvents.some(item => item.uri.toString() === convertSide.toString()));
    assert.ok((await blobDocuments.stat(convertSide)).mtime > before.mtime);
    assert.equal((await blobDocuments.readFile(convertSide)).toString(), 'converted two\n');
    subscription.dispose();
    api.repositories.splice(api.repositories.indexOf(convertRepo), 1);
    closedRepositories.fire(convertRepo);
    console.log('PASS SCM events refresh textconv output when the Git blob is unchanged');
  }
  diffs.length = 0;
  await commands.get('gitpane.refresh')();
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
  fs.writeFileSync(path.join(repoPath, '000-new-after-refresh.txt'), 'new\n');
  pickChanges = choices => choices.find(choice => choice.action === 'refreshRepo');
  await receive({ type: 'menu', kind: 'repo', id: owner.id });
  const refreshedFiles = snapshots.at(-1).repositories[0].groups.flatMap(group => group.files);
  assert.equal(snapshots.at(-1).repositories[0].changesPage.page, 0, 'targeted refresh must immediately refresh detailed changes');
  assert.ok(refreshedFiles.some(item => item.path === '000-new-after-refresh.txt'));
  console.log('PASS hidden Git API changes, paged native diff, unrelated edits, stale selection, and targeted refresh');

  await receive({ type: 'searchChanges', id: owner.id, query: 'file-12', direction: 'first' });
  let searched = snapshots.at(-1).repositories.find(item => item.id === owner.id);
  assert.equal(searched.changesPage.search.phase, 'ready', searched.changesPage.search.error);
  assert.equal(searched.changesPage.search.path, 'file-120.txt');
  assert.equal(searched.changesPage.page, 0);
  assert.ok(searched.changesPage.search.count > 1);
  await receive({ type: 'searchChanges', id: owner.id, query: 'file-12', direction: 'previous' });
  searched = snapshots.at(-1).repositories.find(item => item.id === owner.id);
  assert.equal(searched.changesPage.search.path, 'file-129.txt');
  assert.equal(searched.changesPage.page, 1);
  const match = searched.groups.flatMap(group => group.files).find(item => item.path === 'file-129.txt');
  assert.ok(match, 'search match must appear on the returned page');
  const beforeSearchDiff = diffs.length;
  await receive({ type: 'openChange', id: match.id });
  assert.equal(diffs.length, beforeSearchDiff + 1, errors.at(-1));
  assert.equal(diffs.at(-1)[0].scheme, 'gitpane-empty');
  assert.equal(diffs.at(-1)[1].fsPath, path.join(repoPath, 'file-129.txt'));
  await receive({ type: 'searchChanges', id: owner.id, query: '', direction: 'first' });
  await receive({ type: 'changesPage', id: owner.id, direction: 'previous' });
  assert.equal(snapshots.at(-1).repositories.find(item => item.id === owner.id).changesPage.page, 0);
  console.log('PASS real snapshot search wraps across pages and opens a native file diff');

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

  pickChanges = choices => {
    assert.ok(!choices.some(choice => choice.action === 'removeWorkspaceWorktree'), 'the main repository must not offer self-removal');
    return undefined;
  };
  await receive({ type: 'menu', kind: 'repo', id: remaining.id });

  const lonePath = path.join(temporary, 'lone-worktree');
  git('worktree', 'add', '-qb', 'feature/lone', lonePath);
  const loneRepo = { rootUri: uri(lonePath), kind: 'worktree', state: { HEAD: { name: 'feature/lone',
    commit: gitAt(lonePath, 'rev-parse', 'HEAD') }, onDidChange: event,
  workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  api.repositories.push(loneRepo);
  openedRepositories.fire(loneRepo);
  vscode.workspace.workspaceFolders = [{ uri: uri(lonePath) }];
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  let lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  assert.ok(lone && !snapshots.at(-1).repositories.some(item => item.name === 'repo'), 'only the linked worktree is in scope');
  pickChanges = choices => choices.find(choice => choice.action === 'removeWorkspaceWorktree');
  confirmAction = false;
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  assert.match(confirmations.at(-1), /removes the workspace folder on disk.*remains listed/i);
  assert.ok(fs.existsSync(lonePath), 'cancelling keeps the worktree');
  confirmAction = true;
  const nestedWorkspace = path.join(lonePath, 'nested-workspace');
  fs.mkdirSync(nestedWorkspace);
  vscode.workspace.workspaceFolders = [{ uri: uri(lonePath) }, { uri: uri(nestedWorkspace) }];
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  const beforeNestedWorkspace = errors.length, beforeNestedConfirmation = confirmations.length;
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  assert.equal(errors.length, beforeNestedWorkspace + 1);
  assert.match(errors.at(-1), /worktree containing another workspace folder/i);
  assert.equal(confirmations.length, beforeNestedConfirmation, 'nested workspace is refused before confirmation');
  assert.ok(fs.existsSync(nestedWorkspace));
  vscode.workspace.workspaceFolders = [{ uri: uri(lonePath) }];
  workspaceChanged.fire({});
  fs.rmdirSync(nestedWorkspace);
  fs.writeFileSync(path.join(lonePath, 'dirty.txt'), 'dirty\n');
  await commands.get('gitpane.refresh')();
  lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  const beforeDirty = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  assert.equal(errors.length, beforeDirty + 1);
  assert.match(errors.at(-1), /modified|dirty|untracked/i);
  fs.unlinkSync(path.join(lonePath, 'dirty.txt'));
  git('worktree', 'lock', lonePath);
  await commands.get('gitpane.refresh')();
  lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  const beforeLocked = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  assert.equal(errors.length, beforeLocked + 1);
  assert.match(errors.at(-1), /locked/i);
  git('worktree', 'unlock', lonePath);
  await commands.get('gitpane.refresh')();
  lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  onConfirm = () => { vscode.workspace.workspaceFolders = [{ uri: uri(path.dirname(repoPath)) }]; workspaceChanged.fire({}); };
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  onConfirm = () => {};
  assert.ok(fs.existsSync(lonePath), 'a workspace change during confirmation prevents removal');
  vscode.workspace.workspaceFolders = [{ uri: uri(lonePath) }]; workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  lone = snapshots.at(-1).repositories.find(item => item.name === 'lone-worktree');
  await receive({ type: 'menu', kind: 'repo', id: lone.id });
  assert.equal(fs.existsSync(lonePath), false, errors.at(-1));
  assert.equal(vscode.workspace.workspaceFolders[0].uri.fsPath, lonePath, 'Gitpane does not edit workspace folders');
  assert.ok(fs.existsSync(repoPath), 'the main repository stays intact');
  vscode.workspace.workspaceFolders = [{ uri: uri(path.dirname(repoPath)) }, { uri: uri(lonePath) }];
  workspaceChanged.fire({});
  await commands.get('gitpane.refresh')();
  assert.ok(snapshots.at(-1).repositories.some(item => item.name === 'repo' && !item.statusUnavailable));
  assert.ok(snapshots.at(-1).errors.some(message => message.includes(lonePath) && /unavailable/i.test(message)));
  console.log('PASS lone worktree removal, cancellation, dirty and locked refusal, stale scope, and workspace recovery');

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
  childRepo.buffer = async (ref, relative) => {
    const result = spawnSync('git', ['-C', childPath, 'show', '--textconv', `${ref}:${relative}`],
      { env: isolatedGitEnv(), maxBuffer: 8 * 1024 * 1024 });
    if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr.toString());
    return result.stdout;
  };
  const availableSubmodules = new Map([[childPath, childRepo]]);
  api.openRepository = async target => {
    const submodule = availableSubmodules.get(target.fsPath);
    if (!submodule) throw new Error(`Unexpected repository ${target.fsPath}`);
    if (!api.repositories.includes(submodule)) { api.repositories.push(submodule); openedRepositories.fire(submodule); }
    return submodule;
  };
  await commands.get('gitpane.refresh')();
  let parentAfterSubmodule = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: parentAfterSubmodule.id });
  parentAfterSubmodule = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  const submodule = parentAfterSubmodule.groups.flatMap(group => group.files).find(item => item.path === 'child');
  assert.ok(submodule, 'dirty in-workspace submodule must appear in the parent changes');
  assert.equal(submodule.submoduleDetails?.state, 'dirty');
  assert.ok(submodule.submoduleDetails.head?.branch || submodule.submoduleDetails.head?.detached);
  assert.equal(typeof submodule.submoduleDetails.warn.pointer_unreachable, 'boolean');
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
    if (/^file-\d{3}\.txt$/.test(name) || ['nested-repo', 'unrelated.txt', '000-new-after-refresh.txt', '000-link'].includes(name)) {
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
  assert.equal(warningGroup.files[0].submoduleDetails?.warn.pointer_unreachable, true);
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
  const pointerRepoPath = path.join(path.dirname(repoPath), 'pointer-only');
  fs.mkdirSync(pointerRepoPath);
  gitAt(pointerRepoPath, 'init', '-q');
  gitAt(pointerRepoPath, 'update-index', '--add', '--cacheinfo', '160000', pointerA, 'child');
  gitAt(pointerRepoPath, 'commit', '-qm', 'pointer root');
  gitAt(pointerRepoPath, 'update-index', '--force-remove', 'child');
  fs.mkdirSync(path.join(pointerRepoPath, 'child', '.git'), { recursive: true });
  const pointerRepo = { rootUri: uri(pointerRepoPath), state: { HEAD: { commit: gitAt(pointerRepoPath, 'rev-parse', 'HEAD') },
    onDidChange: event, submodules: [], workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {} };
  api.repositories.push(pointerRepo); openedRepositories.fire(pointerRepo);
  await commands.get('gitpane.refresh')();
  let pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  assert.equal(pointerOnly.statusUnavailable, true, 'a corrupt checkout must not be reported as clean');
  assert.ok(snapshots.at(-1).errors.some(message => /child/.test(message) && /cannot open repository/i.test(message)),
    'the overview reports the unavailable corrupt child');
  await receive({ type: 'loadChanges', id: pointerOnly.id });
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  assert.equal(pointerOnly.changesPage.state, 'error');
  assert.match(pointerOnly.changesPage.error, /cannot open repository/i);
  fs.rmSync(path.join(pointerRepoPath, 'child'), { recursive: true });
  await commands.get('gitpane.refresh')();
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  await receive({ type: 'loadChanges', id: pointerOnly.id });
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  const removedPointer = pointerOnly.groups.flatMap(group => group.files).find(item => item.path === 'child');
  assert.ok(removedPointer, 'deleted gitlink must appear after its checkout is removed');
  const beforeRemovedDiff = diffs.length, beforeRemovedTerminals = terminals.length, beforeRemovedRepos = api.repositories.length;
  await receive({ type: 'openChange', id: removedPointer.id });
  assert.equal(diffs.length, beforeRemovedDiff + 1, errors.at(-1));
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[0]), `Subproject commit ${pointerA}\n`);
  assert.equal(diffs.at(-1)[1].scheme, 'gitpane-empty');
  assert.equal(terminals.length, beforeRemovedTerminals);
  assert.equal(api.repositories.length, beforeRemovedRepos, 'pointer comparison must not open a child checkout');
  gitAt(pointerRepoPath, 'reset', '-q', 'HEAD', '--', 'child');
  fs.writeFileSync(path.join(pointerRepoPath, 'child'), 'replacement text\n');
  await commands.get('gitpane.refresh')();
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  await receive({ type: 'loadChanges', id: pointerOnly.id });
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  const changedPointer = pointerOnly.groups.find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(changedPointer, 'working gitlink to file must appear in the overview');
  const beforeChangedDiff = diffs.length, beforeChangedTerminals = terminals.length;
  await receive({ type: 'openChange', id: changedPointer.id });
  assert.equal(diffs.length, beforeChangedDiff + 1, errors.at(-1));
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[0]), `Subproject commit ${pointerA}\n`);
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[1]), 'replacement text\n');
  fs.writeFileSync(path.join(pointerRepoPath, 'child'), 'replacement next\n');
  await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[1]), /no longer|changed/i);
  fs.unlinkSync(path.join(pointerRepoPath, 'child'));
  await commands.get('gitpane.refresh')();
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  await receive({ type: 'loadChanges', id: pointerOnly.id });
  pointerOnly = snapshots.at(-1).repositories.find(item => item.name === 'pointer-only');
  const workingDeleted = pointerOnly.groups.find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(workingDeleted, 'unstaged gitlink deletion must appear in the overview');
  await receive({ type: 'openChange', id: workingDeleted.id });
  assert.equal(diffs.length, beforeChangedDiff + 2, errors.at(-1));
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[0]), `Subproject commit ${pointerA}\n`);
  assert.equal(diffs.at(-1)[1].scheme, 'gitpane-empty');
  assert.equal(terminals.length, beforeChangedTerminals);
  console.log('PASS unstaged gitlink to file and deletion use guarded native diffs');
  api.repositories = api.repositories.filter(item => item !== pointerRepo); closedRepositories.fire(pointerRepo);
  await commands.get('gitpane.refresh')();
  console.log('PASS corrupt checkout is reported and removed checkout opens a staged pointer diff');
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
  assert.equal((await documents.get('gitpane-blob').readFile(diffs.at(-1)[0])).toString(), 'committed locally\n');
  assert.equal((await documents.get('gitpane-blob').readFile(diffs.at(-1)[1])).toString(), 'staged child commit\n');
  await receive({ type: 'openChange', id: workingPointer.id });
  assert.equal(diffs.length, beforePointerDiffs + 2, errors.at(-1));
  assert.equal((await documents.get('gitpane-blob').readFile(diffs.at(-1)[0])).toString(), 'staged child commit\n');
  assert.equal((await documents.get('gitpane-blob').readFile(diffs.at(-1)[1])).toString(), 'working child commit\n');
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
  const commitSubject = git('show', '-s', '--format=%s', pointerCommit);
  for (const [index, expected] of [[0, pointerCommit], [1, pointerCommit.slice(0, 7)], [2, commitSubject]]) {
    pickChanges = choices => choices[index];
    await graphReceive({ type: 'commitActions', oid: pointerCommit });
    assert.equal(copied.at(-1), expected, errors.at(-1));
  }
  await graphReceive({ type: 'copyCommitSummary', oid: pointerCommit });
  assert.equal(copied.at(-1), `${pointerCommit.slice(0, 7)} ${commitSubject}`);
  await graphReceive({ type: 'copyCommitFile', id: committedPointer.id });
  assert.equal(copied.at(-1), 'child');
  console.log('PASS graph commit and file copy actions use actual Git values');
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

  const previousReviewBase = git('rev-parse', 'refs/remotes/origin/main');
  git('update-ref', 'refs/remotes/origin/main', git('rev-parse', 'HEAD^'));
  await commands.get('gitpane.refresh')();
  const reviewRoot = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  const parkedChild = `${childPath}-parked`;
  fs.renameSync(childPath, parkedChild);
  try {
    pickChanges = choices => choices.find(choice => choice.action === 'reviewBranch') ||
      choices.find(choice => choice.file?.path === 'child');
    const beforeReviewDiffs = diffs.length, beforeReviewTerminals = terminals.length;
    await receive({ type: 'menu', kind: 'repo', id: reviewRoot.id });
    assert.equal(diffs.length, beforeReviewDiffs + 1, errors.at(-1));
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[0]), `Subproject commit ${pointerA}\n`);
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(diffs.at(-1)[1]), `Subproject commit ${pointerB}\n`);
    assert.equal(terminals.length, beforeReviewTerminals);
  } finally {
    fs.renameSync(parkedChild, childPath);
    git('update-ref', 'refs/remotes/origin/main', previousReviewBase);
  }
  console.log('PASS branch review opens a native gitlink diff without an initialized child or terminal');

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
    onDidChange: event, submodules: [], workingTreeChanges: [], indexChanges: [], mergeChanges: [] }, async status() {},
    async buffer(ref, relative) { return gitBuffer(grandPath, ref, relative); } };
  availableSubmodules.set(grandPath, grandRepo);
  childRepo.state.submodules = [{ path: 'nested', url: grandSource }];
  if (!api.repositories.includes(childRepo)) { api.repositories.push(childRepo); openedRepositories.fire(childRepo); }
  const historicalPointer = gitAt(childPath, 'rev-parse', 'HEAD:nested');
  const historicalChildHead = gitAt(childPath, 'rev-parse', 'HEAD');
  const historicalParked = `${grandPath}-historical-parked`;
  const openBeforeHistorical = api.openRepository;
  let historicalNestedOpens = 0;
  api.openRepository = async target => {
    if (target.fsPath === grandPath) historicalNestedOpens++;
    return openBeforeHistorical(target);
  };
  fs.renameSync(grandPath, historicalParked);
  try {
    const openHistorical = async (oldOid, newOid, text = {}) => {
      await commands.get('gitpane.refresh')();
      const owner = snapshots.at(-1).repositories.find(item => item.name === 'repo');
      await receive({ type: 'loadChanges', id: owner.id });
      const changedChild = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
        .find(group => group.id === 'working')?.files.find(item => item.path === 'child');
      assert.ok(changedChild, 'parent must expose its changed child');
      pickChanges = choices => {
        const matching = choices.filter(choice => choice.kind === 'pointer' && choice.file.path === 'nested');
        assert.equal(matching.length, 1, 'a historical type change must be one comparison');
        return matching[0];
      };
      const before = diffs.length, terminalsBefore = terminals.length;
      await receive({ type: 'openChange', id: changedChild.id });
      assert.equal(diffs.length, before + 1, errors.at(-1));
      const [left, right] = diffs.at(-1);
      for (const [uri, oid, expectedText] of [[left, oldOid, text.old], [right, newOid, text.new]]) {
        if (expectedText !== undefined) {
          assert.equal(uri.scheme, 'gitpane-blob');
          const provider = documents.get('gitpane-blob');
          const bytes = await provider.readFile(uri);
          assert.deepEqual(bytes, Buffer.isBuffer(expectedText) ? expectedText : Buffer.from(expectedText));
          assert.ok((await provider.stat(uri)).size >= 0);
        }
        else if (oid) assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(uri), `Subproject commit ${oid}\n`);
        else assert.equal(uri.scheme, 'gitpane-empty');
      }
      assert.equal(historicalNestedOpens, 0, 'historical pointer comparison must not open the nested checkout');
      assert.equal(terminals.length, terminalsBefore);
      return oldOid ? left : right;
    };
    const document = await openHistorical(null, historicalPointer);
    git('add', 'child');
    await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(document), /no longer|changed/i);
    fs.writeFileSync(path.join(grandSource, 'nested.txt'), 'later committed nested change\n');
    gitAt(grandSource, 'add', 'nested.txt');
    gitAt(grandSource, 'commit', '-qm', 'later nested commit');
    const laterPointer = gitAt(grandSource, 'rev-parse', 'HEAD');
    gitAt(childPath, 'update-index', '--cacheinfo', '160000', laterPointer, 'nested');
    gitAt(childPath, 'commit', '-qm', 'update nested pointer');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(historicalPointer, laterPointer);
    git('add', 'child');
    const historicalBlobPath = path.join(temporary, 'historical-blob');
    fs.writeFileSync(historicalBlobPath, 'historic replacement\n');
    const historicalBlob = gitAt(childPath, 'hash-object', '-w', historicalBlobPath);
    gitAt(childPath, 'update-index', '--cacheinfo', '100644', historicalBlob, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace nested pointer with file');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(laterPointer, null, { new: 'historic replacement\n' });
    git('add', 'child');
    gitAt(childPath, 'update-index', '--cacheinfo', '160000', laterPointer, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace file with nested pointer');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(null, laterPointer, { old: 'historic replacement\n' });
    git('add', 'child');
    const binary = Buffer.alloc(1024 * 1024 + 31);
    for (let index = 0; index < binary.length; index++) binary[index] = index % 256;
    fs.writeFileSync(historicalBlobPath, binary);
    const binaryOid = gitAt(childPath, 'hash-object', '-w', historicalBlobPath);
    gitAt(childPath, 'update-index', '--cacheinfo', '100644', binaryOid, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace nested pointer with binary');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(laterPointer, null, { new: binary });
    git('add', 'child');
    gitAt(childPath, 'update-index', '--cacheinfo', '160000', laterPointer, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace binary with nested pointer');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(null, laterPointer, { old: binary });
    git('add', 'child');
    if (process.platform !== 'win32') {
      const attributes = path.join(childPath, '.gitattributes');
      fs.writeFileSync(attributes, 'nested diff=gitpane-test\n');
      gitAt(childPath, 'config', 'diff.gitpane-test.textconv', 'sed y/abcdefghijklmnopqrstuvwxyz/ABCDEFGHIJKLMNOPQRSTUVWXYZ/');
      gitAt(childPath, 'update-index', '--cacheinfo', '100644', historicalBlob, 'nested');
      gitAt(childPath, 'commit', '-qm', 'replace nested pointer with converted text');
      childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
      await openHistorical(laterPointer, null, { new: 'HISTORIC REPLACEMENT\n' });
      git('add', 'child');
      gitAt(childPath, 'update-index', '--cacheinfo', '160000', laterPointer, 'nested');
      gitAt(childPath, 'commit', '-qm', 'replace converted text with nested pointer');
      childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
      fs.unlinkSync(attributes);
      git('add', 'child');
    }
    fs.writeFileSync(historicalBlobPath, '../historic-target');
    const historicalSymlink = gitAt(childPath, 'hash-object', '-w', historicalBlobPath);
    gitAt(childPath, 'update-index', '--cacheinfo', '120000', historicalSymlink, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace nested pointer with symlink');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(laterPointer, null, { new: '../historic-target' });
    git('add', 'child');
    gitAt(childPath, 'update-index', '--cacheinfo', '160000', laterPointer, 'nested');
    gitAt(childPath, 'commit', '-qm', 'replace symlink with nested pointer');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(null, laterPointer, { old: '../historic-target' });
    git('add', 'child');
    gitAt(childPath, 'update-index', '--force-remove', 'nested');
    gitAt(childPath, 'commit', '-qm', 'remove nested pointer');
    childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
    await openHistorical(laterPointer, null);
    gitAt(childPath, 'update-ref', 'HEAD', historicalChildHead);
    gitAt(childPath, 'read-tree', 'HEAD');
    childRepo.state.HEAD.commit = historicalChildHead;
    git('reset', '-q', 'HEAD', '--', 'child');
  } finally {
    api.openRepository = openBeforeHistorical;
    fs.renameSync(historicalParked, grandPath);
  }
  console.log('PASS historical nested pointers and file/symlink type changes open without a nested checkout and reject changed ancestors');
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
  let nestedWorking = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
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
  gitAt(childPath, 'update-index', '--force-remove', 'nested');
  if (api.repositories.includes(grandRepo)) {
    api.repositories = api.repositories.filter(item => item !== grandRepo);
    closedRepositories.fire(grandRepo);
  }
  await commands.get('gitpane.refresh')();
  const nestedOwner = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: nestedOwner.id });
  nestedWorking = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
    .find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(nestedWorking);
  let nestedCheckoutOpens = 0;
  const previousOpenRepository = api.openRepository;
  api.openRepository = async target => {
    if (target.fsPath === grandPath) nestedCheckoutOpens++;
    return previousOpenRepository(target);
  };
  pickChanges = choices => choices.find(choice => choice.kind === 'browse') ||
    choices.find(choice => choice.record?.path === 'nested' && choice.record.group === 'index');
  const beforeNestedPointer = diffs.length, beforeNestedPointerTerminals = terminals.length;
  await receive({ type: 'openChange', id: nestedWorking.id });
  assert.equal(diffs.length, beforeNestedPointer + 1, errors.at(-1));
  assert.equal(nestedCheckoutOpens, 0, 'nested pointer comparison must not open the child checkout');
  assert.equal(terminals.length, beforeNestedPointerTerminals);
  const nestedPointerDoc = diffs.at(-1)[0];
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(nestedPointerDoc),
    `Subproject commit ${gitAt(childPath, 'rev-parse', 'HEAD:nested')}\n`);
  assert.equal(diffs.at(-1)[1].scheme, 'gitpane-empty');
  git('add', 'child');
  await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(nestedPointerDoc), /no longer|changed/i);
  git('reset', '-q', 'HEAD', '--', 'child');
  gitAt(childPath, 'read-tree', 'HEAD');
  api.openRepository = previousOpenRepository;
  await commands.get('gitpane.refresh')();
  const restoredNestedOwner = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: restoredNestedOwner.id });
  nestedWorking = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
    .find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(nestedWorking);
  console.log('PASS nested staged gitlink pointer diff rejects stale ancestors');
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
  assert.equal(diffs.length, beforeNestedPointer + 1, 'pinning must not open a terminal or diff');
  assert.equal(terminals.length, beforeNestedTerminals);
  await commands.get('gitpane.refresh')();
  let pinnedRow = snapshots.at(-1).repositories.find(item => item.name === 'nested');
  assert.ok(pinnedRow?.pinned, 'nested pin is visible in the overview');
  assert.equal(pinnedRow.statusUnavailable, false, 'pinned repository status must be verified before hiding');
  const nestedStatusKey = grandRepo.rootUri.toString();
  const pinnedStatus = provider.tree.backendStatuses.get(nestedStatusKey);
  assert.match(pinnedStatus.repo_identity, /^[a-f0-9]{64}$/);
  provider.tree.backendStatuses.delete(nestedStatusKey);
  provider.tree.changed();
  let repositoryChoices;
  pickChanges = choices => { repositoryChoices = choices.map(choice => choice.action); return choices.find(choice => choice.action === 'hideRepository'); };
  const errorsBeforePendingHide = errors.length;
  await receive({ type: 'menu', kind: 'repo', id: pinnedRow.id });
  assert.deepEqual(repositoryChoices, ['refreshRepo'], 'pending status offers only a local refresh');
  assert.equal(errors.length, errorsBeforePendingHide);
  assert.ok(snapshots.at(-1).repositories.some(item => item.name === 'nested'));
  provider.tree.backendStatuses.set(nestedStatusKey, pinnedStatus);
  provider.tree.changed();
  pinnedRow = snapshots.at(-1).repositories.find(item => item.name === 'nested');
  await receive({ type: 'menu', kind: 'repo', id: pinnedRow.id });
  assert.ok(repositoryChoices.includes('hideRepository'), 'verified status offers the hide action');
  assert.ok(!snapshots.at(-1).repositories.some(item => item.name === 'nested'));
  assert.ok(workspaceState.get('gitpane.hiddenRepositories').repositories.some(item => item.repo_identity === pinnedStatus.repo_identity),
    'hiding stores the verified repository identity');
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
  assert.equal(diffs.length, beforeNestedPointer + 1, 'stale ancestor cannot open a nested diff');
  assert.match(errors.at(-1), /changed|no longer/i);
  console.log('PASS nested working submodule file opens a native diff and rejects changed ancestors');

  gitAt(childPath, 'config', '-f', '.gitmodules', 'submodule.blocked.path', 'blocked/nested');
  gitAt(childPath, 'config', '-f', '.gitmodules', 'submodule.blocked.url', grandSource);
  gitAt(childPath, 'add', '--', '.gitmodules');
  gitAt(childPath, 'update-index', '--add', '--cacheinfo', '160000', historicalPointer, 'blocked/nested');
  gitAt(childPath, 'commit', '-qm', 'add blocked nested pointer');
  childRepo.state.HEAD.commit = gitAt(childPath, 'rev-parse', 'HEAD');
  fs.writeFileSync(path.join(childPath, 'blocked'), 'replacement ancestor\n');
  await commands.get('gitpane.refresh')();
  const blockedOwner = snapshots.at(-1).repositories.find(item => item.name === 'repo');
  await receive({ type: 'loadChanges', id: blockedOwner.id });
  const blockedParent = snapshots.at(-1).repositories.find(item => item.name === 'repo').groups
    .find(group => group.id === 'working')?.files.find(item => item.path === 'child');
  assert.ok(blockedParent, 'parent must expose the child with a blocked nested gitlink');
  pickChanges = choices => choices.find(choice => choice.kind === 'browse') ||
    choices.find(choice => choice.record?.path === 'blocked/nested' && choice.record.group === 'working');
  const beforeBlockedDiffs = diffs.length, beforeBlockedTerminals = terminals.length;
  await receive({ type: 'openChange', id: blockedParent.id });
  assert.equal(diffs.length, beforeBlockedDiffs + 1, errors.at(-1));
  const blockedPointer = diffs.at(-1)[0];
  assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(blockedPointer),
    `Subproject commit ${historicalPointer}\n`);
  assert.equal(diffs.at(-1)[1].scheme, 'gitpane-empty');
  assert.equal(terminals.length, beforeBlockedTerminals);
  git('add', 'child');
  await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(blockedPointer), /no longer|changed/i);
  console.log('PASS nested deleted gitlink opens despite a blocking ancestor and rejects changed parents');
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
