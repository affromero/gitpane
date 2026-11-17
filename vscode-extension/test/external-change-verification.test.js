'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { randomUUID } = require('node:crypto');
const { BackendManager } = require('../src/backend-manager');
const { BridgeClient } = require('../src/bridge-client');

const server = String.raw`
const fs = require('node:fs');
const readline = require('node:readline');
let roots = [], sources = [], snapshot, sequence = 0;
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
readline.createInterface({input: process.stdin}).on('line', line => {
  const request = JSON.parse(line), id = request.id;
  if (request.method === 'initialize') {
    roots = request.params.workspace_roots;
    sources = request.params.workspace_root_sources || roots;
    send({id, result: {version: 1, capabilities: {unique_changes_snapshots: true}}});
  } else if (request.method === 'workspace_root_identities') {
    send({id, result: {roots: roots.map((root, index) => ({path: root, source: sources[index], identity: 'a'.repeat(64)}))}});
  } else if (request.method === 'external_worktree_read') {
    const { method, params } = request.params;
    if (params.repo !== request.params.selection.path &&
        request.params.selection.chain?.map(link => link.path).join('/') !== 'child/nested') {
      send({id, error: {code: 'stale_target', message: 'missing submodule chain'}}); return;
    }
    if (method === 'changes_begin') {
      snapshot = 'snapshot-' + (++sequence);
      send({id, result: {snapshot, revision: 'a'.repeat(64), cursor: 0,
        previous_cursor: null, next_cursor: 1, changes: []}});
    } else if (method === 'changes_find') {
      process.stderr.write('find-started\n');
      const reply = () => {
        if (!fs.existsSync(process.env.GITPANE_TEST_FIND_GATE)) return setTimeout(reply, 10);
        if (params.snapshot !== snapshot) send({id, error: {code: 'snapshot_stale', message: 'snapshot replaced'}});
        else send({id, result: {change: {path: params.path, group: params.group}}});
      };
      reply();
    } else if (method === 'changes_next') {
      if (params.snapshot !== snapshot) send({id, error: {code: 'snapshot_stale', message: 'snapshot replaced'}});
      else send({id, result: {snapshot, revision: 'a'.repeat(64), cursor: params.cursor,
        previous_cursor: 0, next_cursor: null, changes: []}});
    } else if (method === 'changes_release') {
      if (params.snapshot === snapshot) snapshot = undefined;
      send({id, result: {}});
    }
  }
});`;

class EventEmitter {
  constructor() { this.event = () => ({ dispose() {} }); }
  fire() {}
  dispose() {}
}

test('outside-worktree verification preserves snapshots during refresh and rejects revoked access', async t => {
  const gate = path.join(os.tmpdir(), `gitpane-find-${randomUUID()}`);
  fs.writeFileSync(gate, 'ready');
  const root = path.resolve('workspace');
  let notifyFind;
  const workspace = { workspaceFolders: [{ uri: { scheme: 'file', fsPath: root } }],
    getConfiguration: () => ({ get: () => '' }),
    onDidChangeWorkspaceFolders: () => ({ dispose() {} }), onDidChangeConfiguration: () => ({ dispose() {} }) };
  const manager = new BackendManager({ extensionPath: path.resolve('extension') }, {
    vscode: { EventEmitter, workspace }, realpath: async value => path.resolve(value),
    stat: async () => ({ isDirectory: () => true, dev: 1, ino: 1 }),
    output: { append(text) { if (text.includes('find-started')) notifyFind?.(); } },
    createClient: (executable, callbacks) => new BridgeClient(executable, { ...callbacks,
      spawn: () => spawn(process.execPath, ['-e', server], { stdio: ['pipe', 'pipe', 'pipe'],
        env: { ...process.env, GITPANE_TEST_FIND_GATE: gate } }) })
  });
  t.after(() => { manager.dispose(); fs.rmSync(gate, { force: true }); });
  await manager.ready();
  const repo = path.resolve('outside-worktree');
  const selection = { repo: path.join(root, 'repo'), path: repo };
  let valid = true;
  manager.externalSelections.set(repo, { selection, guard: () => valid });
  const record = { path: 'one', group: 'working' };
  const displayed = await manager.beginChanges(repo);
  assert.deepEqual((await manager.verifyChanges(repo, record)).change, record);
  assert.equal((await manager.nextChanges(repo, displayed.snapshot, 1)).snapshot, displayed.snapshot);

  fs.unlinkSync(gate);
  const started = new Promise(resolve => { notifyFind = resolve; });
  const verification = manager.verifyChanges(repo, record);
  await started;
  const refreshed = await manager.beginChanges(repo);
  fs.writeFileSync(gate, 'ready');
  assert.deepEqual((await verification).change, record);
  assert.equal((await manager.nextChanges(repo, refreshed.snapshot, 1)).snapshot, refreshed.snapshot);

  fs.unlinkSync(gate);
  const staleStarted = new Promise(resolve => { notifyFind = resolve; });
  const stale = manager.verifyChanges(repo, record);
  await staleStarted;
  valid = false;
  fs.writeFileSync(gate, 'ready');
  await assert.rejects(stale, error => error.code === 'workspace_changed');
  valid = true;
  assert.deepEqual((await manager.verifyChanges(repo, record)).change, record);

  fs.unlinkSync(gate);
  const replacementStarted = new Promise(resolve => { notifyFind = resolve; });
  const replaced = manager.verifyChanges(repo, record);
  await replacementStarted;
  manager.externalSelections.set(repo, { selection, guard: () => true });
  fs.writeFileSync(gate, 'ready');
  await assert.rejects(replaced, error => error.code === 'workspace_changed');
  assert.deepEqual((await manager.verifyChanges(repo, record)).change, record);

  const child = path.join(repo, 'child', 'nested');
  manager.externalSelections.set(child, { selection: { ...selection, chain: [{ path: 'child' }, { path: 'nested' }] }, guard: () => true });
  const childPage = await manager.beginChanges(child);
  assert.deepEqual((await manager.verifyChanges(child, record)).change, record);
  assert.equal((await manager.nextChanges(child, childPage.snapshot, 1)).snapshot, childPage.snapshot);
});
