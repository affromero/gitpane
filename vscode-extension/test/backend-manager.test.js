'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs/promises');
const os = require('node:os');
const { spawn } = require('node:child_process');
const { BackendManager, normalizeWorkspacePath } = require('../src/backend-manager');
const { BridgeClient } = require('../src/bridge-client');

test('Windows workspace identities compare ordinary and extended drive and UNC paths', () => {
  assert.equal(normalizeWorkspacePath(String.raw`\\?\C:\Work\Repo`, 'win32'), normalizeWorkspacePath(String.raw`c:\work\repo`, 'win32'));
  assert.equal(normalizeWorkspacePath(String.raw`\\?\UNC\Server\Share\Repo`, 'win32'), normalizeWorkspacePath(String.raw`\\server\share\repo`, 'win32'));
  assert.notEqual(normalizeWorkspacePath(String.raw`\\?\UNC\Server\Share\Repo`, 'win32'), normalizeWorkspacePath(String.raw`\\server\other\repo`, 'win32'));
});

const server = String.raw`
const readline = require('node:readline');
let roots = [];
let sources = [];
function send(value) { process.stdout.write(JSON.stringify(value) + '\n'); }
readline.createInterface({input: process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    roots = request.params.workspace_roots;
    sources = request.params.workspace_root_sources || roots;
    send({id: request.id, result: {version: 1, capabilities: {liveness_worker: true}}});
  } else if (request.method === 'workspace_root_identities') {
    send({id: request.id, result: {roots: roots.map((path, index) => ({path, source: sources[index], identity: 'a'.repeat(64)}))}});
  } else if (request.method === 'status' && request.params.repo.endsWith('slow')) {
    const snapshot = [...roots];
    setTimeout(() => send({id: request.id, result: {roots: snapshot}}), 100);
  } else if (request.method === 'status' && request.params.repo.endsWith('bad')) {
    send({id: request.id, error: {code: 'repository_error', message: 'Repository unavailable'}});
  } else if (request.method === 'worktree_plan' && request.params.branch === 'bad branch') {
    send({id: request.id, error: {code: 'invalid_request', message: 'invalid branch name'}});
  } else if (request.method === 'worktree_plan' && request.params.branch === 'offline') {
    process.exit(1);
  } else if (request.method === 'changes_begin') {
    const result = {snapshot: 'sample', revision: 'a'.repeat(64), cursor: 0,
      previous_cursor: null, next_cursor: 1, changes: []};
    if (request.params.repo.endsWith('slow')) setTimeout(() => send({id: request.id, result}), 300);
    else send({id: request.id, result});
  } else if (request.method === 'changes_next') {
    send({id: request.id, result: {snapshot: request.params.snapshot, revision: 'a'.repeat(64),
      cursor: request.params.cursor, previous_cursor: 0, next_cursor: null, changes: []}});
  } else if (request.method === 'changes_find') {
    send({id: request.id, result: {change: {path: request.params.path, group: request.params.group}}});
  } else if (request.method === 'submodule_chain_validate') {
    const result = {valid: true};
    if (request.params.chain.some(link => link.path === 'slow')) setTimeout(() => send({id: request.id, result}), 300);
    else send({id: request.id, result});
  } else if (request.method === 'sessions_list') {
    send({id: request.id, result: {sessions: [request.params.backend + '-session']}});
  } else if (request.method === 'session_liveness') {
    if (request.params.repos.some(repo => repo.endsWith('probe-error'))) {
      send({id: request.id, error: {code: 'session_error', message: 'host probe failed'}}); return;
    }
    const result = {enabled: true, reason: null, repos: request.params.repos.map(repo => ({repo, live: request.params.backend === 'herdr' || repo.endsWith('topic')}))};
    if (request.params.repos.some(repo => repo.endsWith('slow'))) setTimeout(() => send({id: request.id, result}), 300);
    else send({id: request.id, result});
  } else {
    send({id: request.id, result: {roots, method: request.method, include_worktrees: request.params.include_worktrees, summary_only: request.params.summary_only, watched_paths: request.params.watched_paths, first_parent: request.params.first_parent, filters: request.params.filters, collapsed_segments: request.params.collapsed_segments, oid: request.params.oid, path: request.params.path, group: request.params.group, original_path: request.params.original_path, operation: request.params.operation, branch: request.params.branch}});
  }
});

`;

class EventEmitter {
  constructor() {
    this.listeners = new Set();
    this.event = listener => {
      this.listeners.add(listener);
      return { dispose: () => this.listeners.delete(listener) };
    };
  }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function fixture() {
  const folders = new EventEmitter();
  const configuration = new EventEmitter();
  let backendPath = '';
  let sessionBackend = 'auto';
  const root = path.resolve('workspace');
  const workspace = {
    workspaceFolders: [{ uri: { scheme: 'file', fsPath: root } }],
    getConfiguration: () => ({ get: name => name === 'sessionBackend' ? sessionBackend : backendPath }),
    onDidChangeWorkspaceFolders: folders.event,
    onDidChangeConfiguration: configuration.event
  };
  const executed = [];
  const manager = new BackendManager({ extensionPath: path.resolve('extension') }, {
    vscode: { EventEmitter, workspace },
    realpath: async value => path.resolve(value),
    stat: async () => ({ isDirectory: () => true, dev: 1, ino: 1 }),
    createClient: (executable, callbacks) => {
      executed.push(executable);
      return new BridgeClient(executable, {
        ...callbacks,
        spawn: () => spawn(process.execPath, ['-e', server], { stdio: ['pipe', 'pipe', 'pipe'] })
      });
    }
  });
  return {
    manager, workspace, root, executed,
    setFolders(values) {
      workspace.workspaceFolders = values.map(value => ({ uri: { scheme: 'file', fsPath: value } }));
      folders.fire({});
    },
    setBackendPath(value) {
      backendPath = value;
      configuration.fire({ affectsConfiguration: name => name === 'gitpane.backendPath' });
    },
    setSessionBackend(value) {
      sessionBackend = value;
      configuration.fire({ affectsConfiguration: name => name === 'gitpane.sessionBackend' });
    }
  };
}

test('backend initializes workspace roots and reads status and graph', async t => {
  const { manager, root, executed } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  assert.equal(manager.state.status, 'ready');
  assert.equal(executed[0], path.resolve('extension/bin', process.platform === 'win32' ? 'gitpane.exe' : 'gitpane'));
  assert.deepEqual((await manager.status(path.join(root, 'repo'))).roots, [root]);
  assert.equal((await manager.status(path.join(root, 'repo'), false)).include_worktrees, false);
  const compact = await manager.status(path.join(root, 'repo'), false, { summaryOnly: true, watchedPaths: ['link'] });
  assert.equal(compact.summary_only, true);
  assert.deepEqual(compact.watched_paths, ['link']);
  const graph = await manager.graph(path.join(root, 'repo'), true);
  assert.equal((await manager.graphRevision(path.join(root, 'repo'))).method, 'graph_revision');
  assert.equal((await manager.theme()).method, 'theme');
  assert.equal(graph.method, 'graph');
  assert.equal(graph.first_parent, true);
  const filters = { branches: ['topic'], authors: ['Alice'], refs: ['v1'] };
  assert.deepEqual((await manager.graph(path.join(root, 'repo'), false, filters)).filters, filters);
  assert.deepEqual((await manager.graph(path.join(root, 'repo'), false, filters, ['abc'])).collapsed_segments, ['abc']);
  const files = await manager.commitFiles(path.join(root, 'repo'), 'abc123');
  assert.equal(files.method, 'commit_files');
  assert.equal(files.oid, 'abc123');
  assert.equal((await manager.branchReview(path.join(root, 'repo'))).method, 'branch_review');
  const child = await manager.inspectSubmodule(path.join(root, 'repo'), 'child');
  assert.equal(child.path, 'child');
  assert.equal(child.summary_only, true);
  assert.equal((await manager.inspectSubmodule(path.join(root, 'repo'), 'child', 'index')).group, 'index');
  assert.equal((await manager.inspectSubmodule(path.join(root, 'repo'), 'child', 'index', 'old-child')).original_path, 'old-child');
  assert.deepEqual(await manager.fileActionPreview(path.join(root, 'repo'), 'file', 'discard'), {
    roots: [root], method: 'file_action_preview', path: 'file', operation: 'discard'
  });
  assert.deepEqual(await manager.previewFetch(path.join(root, 'repo')), { roots: [root], method: 'repo_fetch_preview' });
  assert.deepEqual(await manager.previewWorktreeRemoval(path.join(root, 'repo'), path.join(root, 'linked')), {
    roots: [root], method: 'worktree_remove_preview', path: path.join(root, 'linked')
  });
  assert.deepEqual(await manager.planWorktree(path.join(root, 'repo'), 'feature/a'), {
    roots: [root], method: 'worktree_plan', branch: 'feature/a'
  });
});

test('theme configuration errors do not mark the repository backend unavailable', async t => {
  const { manager } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  manager.client.theme = async () => { throw Object.assign(new Error('theme missing'), { code: 'theme_error' }); };
  await assert.rejects(manager.theme(), /theme missing/);
  assert.equal(manager.state.status, 'ready');
});

test('session liveness uses the selected host and invalidates stale workspace probes', async t => {
  const { manager, root, setFolders, setSessionBackend } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const repos = [path.join(root, 'main'), path.join(root, 'topic')];
  assert.deepEqual(await manager.sessionLiveness(repos), { enabled: true, reason: null,
    repos: [{ repo: repos[0], live: false }, { repo: repos[1], live: true }] });
  setSessionBackend('herdr');
  assert.deepEqual(await manager.sessionLiveness([repos[0]]), { enabled: true, reason: null,
    repos: [{ repo: repos[0], live: true }] });
  setSessionBackend('tmux');
  await assert.rejects(manager.sessionLiveness([path.join(root, 'probe-error')]), error => error.code === 'session_error');
  assert.equal(manager.state.status, 'ready');
  await assert.rejects(manager.sessionLiveness([path.resolve('outside')]), error => error.code === 'outside_workspace');
  const pending = manager.sessionLiveness([path.join(root, 'slow')]);
  await new Promise(resolve => setTimeout(resolve, 20));
  setFolders([path.resolve('other-workspace')]);
  await assert.rejects(pending, error => error.code === 'workspace_changed');
});

test('unresponsive liveness startup or probe times out, releases its worker, and recovers', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const original = manager.createClient;
  manager.livenessRequestTimeoutMs = 150;
  for (const mode of ['initialize', 'probe']) {
    let closed;
    let sawProbe = false;
    manager.livenessInitializationTimeoutMs = mode === 'initialize' ? 200 : 3000;
    manager.livenessRequestTimeoutMs = 150;
    const script = `const readline = require('node:readline');\nreadline.createInterface({input:process.stdin}).on('line', line => {\nconst request = JSON.parse(line);\nif (request.method === 'initialize' && ${JSON.stringify(mode)} === 'probe') process.stdout.write(JSON.stringify({id: request.id, result: {version:1,capabilities:{liveness_worker:true}}}) + '\\n');\nif (request.method === 'session_liveness') process.stderr.write('probe-started');\n});`;
    manager.createClient = (executable, options) => new BridgeClient(executable, { ...options,
      onStderr: value => { if (value.includes('probe-started')) sawProbe = true; }, spawn: () => {
      const child = spawn(process.execPath, ['-e', script], { stdio: ['pipe', 'pipe', 'pipe'] });
      closed = new Promise(resolve => child.once('close', resolve));
      return child;
    } });
    await assert.rejects(manager.sessionLiveness([path.join(root, 'main')]), error => error.code === 'timeout');
    assert.equal(sawProbe, mode === 'probe', 'the timeout occurs at the intended boundary');
    assert.equal(manager.livenessClient, undefined);
    await closed;
    manager.createClient = original;
    manager.livenessInitializationTimeoutMs = 3000;
    manager.livenessRequestTimeoutMs = 3000;
    assert.equal((await manager.sessionLiveness([path.join(root, 'main')])).enabled, true);
  }
});

test('a backend without cooperative worker capability cannot run a liveness probe', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  for (const capabilities of [undefined, { liveness_worker: 'true' }]) {
    let sawProbe = false;
    const handshake = JSON.stringify({ version: 1, ...(capabilities ? { capabilities } : {}) });
    const script = `const readline = require('node:readline');\nreadline.createInterface({input:process.stdin}).on('line', line => {\nconst request = JSON.parse(line);\nif (request.method === 'initialize') process.stdout.write(JSON.stringify({id:request.id,result:${handshake}}) + '\\n');\nelse if (request.method === 'session_liveness') process.stderr.write('probe-started');\n});`;
    manager.createClient = (executable, options) => new BridgeClient(executable, { ...options,
      onStderr: value => { if (value.includes('probe-started')) sawProbe = true; },
      spawn: () => spawn(process.execPath, ['-e', script], { stdio: ['pipe', 'pipe', 'pipe'] }) });
    await assert.rejects(manager.sessionLiveness([path.join(root, 'main')]), error =>
      error.code === 'unsupported_backend' && /update.*backend/i.test(error.message));
    assert.equal(sawProbe, false);
    assert.equal(manager.livenessClient, undefined);
  }
});

test('disposing while a liveness response is held drains the bounded worker', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  manager.livenessRequestTimeoutMs = 100;
  let enterProbe;
  const probeStarted = new Promise(resolve => { enterProbe = resolve; });
  let closed;
  const script = String.raw`const readline = require('node:readline');
readline.createInterface({input:process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') process.stdout.write(JSON.stringify({id:request.id,result:{version:1,capabilities:{liveness_worker:true}}}) + '\n');
  else if (request.method === 'session_liveness') process.stderr.write('probe-started');
});`;
  manager.createClient = (executable, options) => new BridgeClient(executable, { ...options,
    onStderr: value => { if (value.includes('probe-started')) enterProbe(); }, spawn: () => {
      const child = spawn(process.execPath, ['-e', script], { stdio: ['pipe', 'pipe', 'pipe'] });
      closed = new Promise(resolve => child.once('close', resolve));
      return child;
    } });
  const pending = manager.sessionLiveness([path.join(root, 'main')]);
  await probeStarted;
  manager.dispose();
  await assert.rejects(pending, error => error.code === 'workspace_changed');
  await closed;
  assert.equal(manager.livenessClient, undefined);
});

test('liveness keeps its lease until a timed-out worker finishes signal cleanup', { skip: process.platform === 'win32' }, async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  manager.livenessRequestTimeoutMs = 100;
  let term;
  const signaled = new Promise(resolve => { term = resolve; });
  const script = String.raw`const readline = require('node:readline');
process.on('SIGTERM', () => { process.stderr.write('cleanup-started'); setTimeout(() => process.exit(0), 250); });
readline.createInterface({input:process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') process.stdout.write(JSON.stringify({id:request.id,result:{version:1,capabilities:{liveness_worker:true}}}) + '\n');
});`;
  manager.createClient = (executable, options) => new BridgeClient(executable, { ...options,
    onStderr: value => { if (value.includes('cleanup-started')) term(); },
    spawn: () => spawn(process.execPath, ['-e', script], { stdio: ['pipe', 'pipe', 'pipe'] }) });
  const pending = manager.sessionLiveness([path.join(root, 'main')]);
  await signaled;
  assert.ok(manager.livenessClient, 'the lease remains held during cleanup');
  await assert.rejects(manager.sessionLiveness([path.join(root, 'main')]), error => error.code === 'busy');
  await assert.rejects(pending, error => error.code === 'timeout');
  assert.equal(manager.livenessClient, undefined);
});

test('missing workspace folders do not block remaining roots and do not widen scope', async t => {
  const { manager, root, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const removed = path.resolve('removed-workspace');
  let missing = false;
  let replaced = false;
  const realpath = manager.realpath;
  manager.realpath = async value => {
    if (value === removed && missing) throw Object.assign(new Error('missing'), { code: 'ENOENT' });
    if (value === removed && replaced) return path.resolve('outside-workspace');
    return realpath(value);
  };
  setFolders([root, removed]);
  await manager.ready();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root, removed]);
  missing = true;
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root]);
  assert.deepEqual((await manager.status(path.join(root, 'repo'))).roots, [root]);
  missing = false;
  replaced = true;
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root]);
  replaced = false;
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root, removed], 'original identity restores its scope');
  missing = true;
  setFolders([root, removed]);
  await manager.ready();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root]);
  assert.equal(manager.state.status, 'ready');
});

test('workspace identity survives temporary disappearance and rejects replacement until re-added', async t => {
  const { manager, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'gitpane-roots-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const root = path.join(dir, 'root');
  const missing = path.join(dir, 'missing');
  const saved = path.join(dir, 'saved');
  await fs.mkdir(root);
  manager.realpath = fs.realpath;
  manager.stat = fs.stat;
  setFolders([root, missing]);
  await manager.ready();
  const canonical = await fs.realpath(root);
  assert.deepEqual((await manager.mutationSnapshot()).roots, [canonical]);
  await fs.mkdir(missing);
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [canonical], 'folder missing at startup needs re-adding');
  await fs.rename(root, saved);
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, []);
  await fs.rename(saved, root);
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [canonical], 'renamed-back original is restored');
  await fs.rename(root, saved);
  await fs.mkdir(root);
  await manager.syncWorkspaceRoots();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [], 'replacement directory remains unauthorized');
  setFolders([missing]);
  await manager.ready();
  setFolders([root, missing]);
  await manager.ready();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [canonical], 're-adding explicitly authorizes replacement');
});

test('duplicate workspace aliases remain bound to their sources after one is retargeted', { skip: process.platform === 'win32' }, async t => {
  const { manager, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'gitpane-aliases-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const target = path.join(dir, 'target');
  const outside = path.join(dir, 'outside');
  const first = path.join(dir, 'first');
  const second = path.join(dir, 'second');
  await fs.mkdir(target);
  await fs.mkdir(outside);
  await fs.symlink(target, first);
  await fs.symlink(target, second);
  manager.realpath = fs.realpath;
  manager.stat = fs.stat;
  setFolders([first, second]);
  await manager.ready();
  const canonical = await fs.realpath(target);
  const initial = await manager.mutationSnapshot();
  assert.deepEqual(initial.roots, [canonical, canonical]);
  assert.deepEqual(initial.rootSources, [first, second]);
  await fs.unlink(first);
  await fs.symlink(outside, first);
  await manager.syncWorkspaceRoots();
  const current = await manager.mutationSnapshot();
  assert.deepEqual(current.roots, [canonical]);
  assert.deepEqual(current.rootSources, [second]);
  assert.equal(manager.isCurrentMutation(initial, path.join(canonical, 'repo')), false);
  assert.equal(manager.isCurrentMutation(current, path.join(canonical, 'repo')), true);
});

test('overlapping mutation snapshots wait for workspace scope reinitialization', async t => {
  const { manager, root, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const removed = path.resolve('removed-workspace');
  setFolders([root, removed]);
  await manager.ready();
  const realpath = manager.realpath;
  manager.realpath = async value => {
    if (value === removed) throw Object.assign(new Error('missing'), { code: 'ENOENT' });
    return realpath(value);
  };
  let release, entered;
  const initialized = new Promise(resolve => { entered = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  const initialize = manager.client.initialize.bind(manager.client);
  manager.client.initialize = async (...args) => { entered(); await gate; return initialize(...args); };
  const first = manager.mutationSnapshot();
  const second = manager.mutationSnapshot();
  await initialized;
  release();
  const snapshots = await Promise.all([first, second]);
  assert.deepEqual(snapshots.map(snapshot => snapshot.roots), [[root], [root]]);
  assert.equal(snapshots[0].generation, snapshots[1].generation);
});

test('workspace removal during initialization is retried with the remaining scope', async t => {
  const { manager, root, workspace } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const removed = path.resolve('removed-workspace');
  let missing = false;
  const realpath = manager.realpath;
  manager.realpath = async value => {
    if (value === removed && missing) throw Object.assign(new Error('missing'), { code: 'ENOENT' });
    return realpath(value);
  };
  const initialize = manager.client.initialize.bind(manager.client);
  manager.client.initialize = async (...args) => {
    if (args[0].includes(removed)) {
      missing = true;
      throw Object.assign(new Error('invalid workspace root'), { code: 'invalid_request' });
    }
    return initialize(...args);
  };
  workspace.workspaceFolders = [root, removed].map(fsPath => ({ uri: { scheme: 'file', fsPath } }));
  await manager.reconfigure();
  assert.deepEqual((await manager.mutationSnapshot()).roots, [root]);
  assert.equal(manager.state.status, 'ready');
});

test('submodule validation uses a bounded reusable worker isolated from paging and workspace changes', async t => {
  const { manager, root, executed, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const link = { repo: path.join(root, 'repo'), child_repo: path.join(root, 'repo', 'child'), path: 'child' };
  await manager.beginChanges(link.repo);
  assert.equal(executed.length, 2, 'paging gets a separate worker');
  assert.deepEqual(await manager.validateSubmoduleChain([link]), { valid: true });
  assert.deepEqual(await manager.validateSubmoduleChain([link]), { valid: true });
  assert.equal(executed.length, 3, 'repeated validation reuses its own worker');
  assert.equal((await manager.nextChanges(link.repo, 'sample', 1)).snapshot, 'sample');
  await assert.rejects(manager.validateSubmoduleChain(Array(33).fill(link)), error => error.code === 'invalid_request');
  const realpath = manager.realpath;
  manager.realpath = async value => {
    if (value.endsWith(`${path.sep}missing`)) throw Object.assign(new Error('missing'), { code: 'ENOENT' });
    return realpath(value);
  };
  assert.deepEqual(await manager.validateSubmoduleChain([{ ...link, child_repo: path.join(root, 'missing') }]), { valid: false });
  const slow = { ...link, path: 'slow' };
  const pending = Array.from({ length: 4 }, () => manager.validateSubmoduleChain([slow]));
  await assert.rejects(manager.validateSubmoduleChain([slow]), error => error.code === 'busy');
  const results = Promise.all(pending.map(promise => assert.rejects(promise, error => error.code === 'workspace_changed')));
  await new Promise(resolve => setTimeout(resolve, 30));
  const nextRoot = path.resolve('next-workspace');
  setFolders([nextRoot]);
  await results;
  const next = { ...link, repo: path.join(nextRoot, 'repo'), child_repo: path.join(nextRoot, 'repo', 'child') };
  assert.deepEqual(await manager.validateSubmoduleChain([next]), { valid: true });
  assert.equal(executed.length, 5, 'reconfiguration starts fresh main and scoped validation workers');
  const interrupted = manager.validateSubmoduleChain([{ ...next, path: 'slow' }]);
  const rejected = assert.rejects(interrupted, error => error.code === 'workspace_changed');
  await new Promise(resolve => setTimeout(resolve, 30));
  manager.dispose();
  await rejected;
  assert.equal(manager.validationClient, undefined);
});

test('session discovery is limited to current workspace repositories', async t => {
  const { manager, root, setFolders } = fixture();
  t.after(() => manager.dispose());
  const repo = path.join(root, 'repo');
  assert.deepEqual(await manager.listSessions(repo, 'tmux'), { sessions: ['tmux-session'] });
  await assert.rejects(manager.listSessions(path.resolve('outside'), 'tmux'), error => error.code === 'workspace_changed');
  await assert.rejects(manager.listSessions(repo, 'unknown'), error => error.code === 'invalid_request');
  setFolders([path.resolve('other-workspace')]);
  await assert.rejects(manager.listSessions(repo, 'herdr'), error => error.code === 'workspace_changed');
});

test('workspace changes reject stale reads and authorize the new roots', async t => {
  const { manager, root, setFolders } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const stale = manager.status(path.join(root, 'slow'));
  const queued = manager.status(path.join(root, 'also-slow'));
  const rejected = Promise.all([
    assert.rejects(stale, error => error.code === 'workspace_changed'),
    assert.rejects(queued, error => error.code === 'workspace_changed')
  ]);
  await new Promise(resolve => setTimeout(resolve, 20));
  const nextRoot = path.resolve('other-workspace');
  setFolders([nextRoot]);
  await rejected;
  assert.deepEqual((await manager.status(path.join(nextRoot, 'repo'))).roots, [nextRoot]);
});

test('mutation snapshots track configured executable and current workspace scope', async t => {
  const { manager, root, setFolders, setBackendPath } = fixture();
  t.after(() => manager.dispose());
  const first = await manager.mutationSnapshot();
  assert.deepEqual(first.roots, [root]);
  assert.equal(manager.isCurrentMutation(first, path.join(root, 'repo')), true);
  assert.equal(manager.isCurrentMutation(first, path.resolve('elsewhere')), false);
  first.roots.push(path.resolve('forged'));
  assert.equal(manager.isCurrentMutation(first, path.join(root, 'repo')), false);
  first.roots.pop();
  setFolders([path.resolve('other-workspace')]);
  assert.equal(manager.isCurrentMutation(first, path.join(root, 'repo')), false);
  const second = await manager.mutationSnapshot();
  assert.deepEqual(second.roots, [path.resolve('other-workspace')]);
  setBackendPath('relative');
  await assert.rejects(manager.mutationSnapshot(), error => error.code === 'invalid_backend_path');
  assert.equal(manager.isCurrentMutation(second, path.join(second.roots[0], 'repo')), false);
});

test('invalid explicit backend path surfaces error without using bundled service', async t => {
  const { manager, executed, setBackendPath } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  setBackendPath('relative-service');
  await assert.rejects(manager.ready(), error => error.code === 'invalid_backend_path');
  assert.equal(manager.state.status, 'error');
  assert.equal(executed.length, 1);
  const custom = path.resolve('custom-service');
  setBackendPath(custom);
  await manager.ready();
  assert.equal(manager.state.status, 'ready');
  assert.equal(executed.at(-1), custom);
});

test('backend errors are observable and a later successful request clears the error', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const changes = [];
  const listener = manager.onDidChange(state => changes.push(state.status));
  t.after(() => listener.dispose());
  await assert.rejects(manager.status(path.join(root, 'bad')), error => error.code === 'repository_error');
  assert.equal(manager.state.status, 'error');
  assert.equal((await manager.status(path.join(root, 'good'))).method, 'status');
  assert.equal(manager.state.status, 'ready');
  assert.deepEqual(changes, ['error', 'ready']);
});

test('invalid worktree branches remain action errors while backend failures stay visible', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const repo = path.join(root, 'repo');
  await assert.rejects(manager.planWorktree(repo, 'bad branch'),
    error => error.code === 'invalid_request' && /invalid branch/i.test(error.message));
  assert.equal(manager.state.status, 'ready');
  assert.equal((await manager.status(repo)).method, 'status');
  await assert.rejects(manager.planWorktree(repo, 'offline'), error => error.code === 'service_exited');
  assert.equal(manager.state.status, 'error');
  assert.equal(manager.state.error.code, 'service_exited');
});

test('detailed change scans do not block status and file checks preserve the visible snapshot', async t => {
  const { manager, root } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const repo = path.join(root, 'slow');
  let completed = false;
  const pending = manager.beginChanges(repo).then(value => { completed = true; return value; });
  assert.equal((await manager.status(path.join(root, 'repo'))).method, 'status');
  assert.equal(completed, false);
  const first = await pending;
  const verification = await manager.verifyChanges(repo, {path: 'one', group: 'untracked'});
  assert.equal(verification.page.revision, first.revision);
  assert.deepEqual(verification.change, {path: 'one', group: 'untracked'});
  const next = await manager.nextChanges(repo, first.snapshot, 1);
  assert.equal(next.snapshot, first.snapshot);
  assert.equal(next.cursor, 1);
});

test('cancelling a pending change scan does not start a worker and allows a later scan', async t => {
  const { manager, root, executed } = fixture();
  t.after(() => manager.dispose());
  await manager.ready();
  const repo = path.join(root, 'repo');
  const pending = manager.beginChanges(repo);
  manager.cancelChanges();
  await assert.rejects(pending, error => error.code === 'workspace_changed');
  assert.equal(executed.length, 1);
  assert.equal((await manager.beginChanges(repo)).snapshot, 'sample');
  assert.equal(executed.length, 2);
});

test('disposal rejects pending requests and prevents further reads', async () => {
  const { manager, root } = fixture();
  await manager.ready();
  const pending = manager.status(path.join(root, 'slow'));
  await new Promise(resolve => setTimeout(resolve, 20));
  manager.dispose();
  await assert.rejects(pending, error => error.code === 'disposed');
  await assert.rejects(manager.status(path.join(root, 'repo')), error => error.code === 'disposed');
  assert.equal(manager.state.status, 'disposed');
});
