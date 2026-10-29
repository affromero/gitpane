'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { BackendManager } = require('../src/backend-manager');
const { BridgeClient } = require('../src/bridge-client');

const server = String.raw`
const readline = require('node:readline');
let roots = [];
function send(value) { process.stdout.write(JSON.stringify(value) + '\n'); }
readline.createInterface({input: process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    roots = request.params.workspace_roots;
    send({id: request.id, result: {version: 1}});
  } else if (request.method === 'status' && request.params.repo.endsWith('slow')) {
    const snapshot = [...roots];
    setTimeout(() => send({id: request.id, result: {roots: snapshot}}), 100);
  } else if (request.method === 'status' && request.params.repo.endsWith('bad')) {
    send({id: request.id, error: {code: 'repository_error', message: 'Repository unavailable'}});
  } else if (request.method === 'sessions_list') {
    send({id: request.id, result: {sessions: [request.params.backend + '-session']}});
  } else {
    send({id: request.id, result: {roots, method: request.method, include_worktrees: request.params.include_worktrees, first_parent: request.params.first_parent, filters: request.params.filters, collapsed_segments: request.params.collapsed_segments, oid: request.params.oid, path: request.params.path, operation: request.params.operation}});
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
  const root = path.resolve('workspace');
  const workspace = {
    workspaceFolders: [{ uri: { scheme: 'file', fsPath: root } }],
    getConfiguration: () => ({ get: () => backendPath }),
    onDidChangeWorkspaceFolders: folders.event,
    onDidChangeConfiguration: configuration.event
  };
  const executed = [];
  const manager = new BackendManager({ extensionPath: path.resolve('extension') }, {
    vscode: { EventEmitter, workspace },
    realpath: async value => path.resolve(value),
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
  const graph = await manager.graph(path.join(root, 'repo'), true);
  assert.equal(graph.method, 'graph');
  assert.equal(graph.first_parent, true);
  const filters = { branches: ['topic'], authors: ['Alice'], refs: ['v1'] };
  assert.deepEqual((await manager.graph(path.join(root, 'repo'), false, filters)).filters, filters);
  assert.deepEqual((await manager.graph(path.join(root, 'repo'), false, filters, ['abc'])).collapsed_segments, ['abc']);
  const files = await manager.commitFiles(path.join(root, 'repo'), 'abc123');
  assert.equal(files.method, 'commit_files');
  assert.equal(files.oid, 'abc123');
  assert.equal((await manager.branchReview(path.join(root, 'repo'))).method, 'branch_review');
  assert.equal((await manager.inspectSubmodule(path.join(root, 'repo'), 'child')).path, 'child');
  assert.deepEqual(await manager.fileActionPreview(path.join(root, 'repo'), 'file', 'discard'), {
    roots: [root], method: 'file_action_preview', path: 'file', operation: 'discard'
  });
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
