'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const path = require('node:path');
const { BridgeClient } = require('../src/bridge-client');

// A real child process stands in for the service boundary. It speaks the
// published JSONL protocol and can emit controlled malformed replies.
const server = String.raw`
const readline = require('node:readline');
let roots = [];
let lateReply;
function send(value) { process.stdout.write(JSON.stringify(value) + '\n'); }
readline.createInterface({input: process.stdin}).on('line', line => {
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    roots = request.params.workspace_roots;
    send({id: request.id, result: {version: process.env.MODE === 'version' ? 2 : 1}});
    return;
  }
  if (process.env.MODE === 'malformed') { process.stdout.write('not json\n'); return; }
  if (process.env.MODE === 'invalid_utf8') { process.stdout.write(Buffer.from([123, 34, 120, 34, 58, 34, 255, 34, 125, 10])); return; }
  if (process.env.MODE === 'oversized') { send({id: request.id, result: {text: 'x'.repeat(1024)}}); return; }
  if (process.env.MODE === 'large_status' && request.method === 'status' && !request.params.summary_only) {
    send({id: request.id, result: {files: 'x'.repeat(8192)}}); return;
  }
  if (process.env.MODE === 'crash') { process.exit(9); }
  if (process.env.MODE === 'never') return;
  if (process.env.MODE === 'old' && request.params.summary_only) {
    send({id: request.id, error: {code: 'invalid_request', message: 'unknown field summary_only'}});
    return;
  }
  if (request.params.repo === '/denied') {
    send({id: request.id, error: {code: 'outside_workspace', message: 'Repository is outside the workspace'}});
    return;
  }
  const result = {method: request.method, repo: request.params.repo, roots, include_worktrees: request.params.include_worktrees, summary_only: request.params.summary_only, watched_paths: request.params.watched_paths, first_parent: request.params.first_parent, filters: request.params.filters, collapsed_segments: request.params.collapsed_segments, oid: request.params.oid, path: request.params.path, operation: request.params.operation, branch: request.params.branch};
  if (process.env.MODE === 'reorder' && request.method === 'status') {
    setTimeout(() => send({id: request.id, result}), 80);
  } else if (process.env.MODE === 'late' && request.method === 'status') {
    lateReply = {id: request.id, result};
  } else {
    if (process.env.MODE === 'late' && lateReply) { send(lateReply); lateReply = undefined; }
    send({id: request.id, result});
  }
});
`;

function fixture(mode, options = {}) {
  const client = new BridgeClient(path.join(path.parse(process.execPath).root, 'fixture-gitpane'), {
    ...options,
    spawn: () => spawn(process.execPath, ['-e', server], {
      env: { ...process.env, MODE: mode }, stdio: ['pipe', 'pipe', 'pipe']
    })
  });
  return client;
}

test('status and graph responses are matched to their requests', async t => {
  const client = fixture('reorder');
  t.after(() => client.dispose());
  const repo = path.resolve('repo');
  const root = path.resolve('workspace');
  await client.initialize([root]);
  const [status, graph] = await Promise.all([client.status(repo), client.graph(repo, true)]);
  assert.deepEqual(status, { method: 'status', repo, roots: [root] });
  assert.deepEqual(await client.status(repo, false), { method: 'status', repo, roots: [root], include_worktrees: false });
  assert.deepEqual(await client.status(repo, false, { summaryOnly: true, watchedPaths: ['dir/link'] }),
    { method: 'status', repo, roots: [root], include_worktrees: false, summary_only: true, watched_paths: ['dir/link'] });
  assert.deepEqual(graph, { method: 'graph', repo, roots: [root], first_parent: true });
  const filters = { branches: ['topic'], authors: ['Alice'], refs: ['v1'] };
  assert.deepEqual(await client.graph(repo, false, filters), { method: 'graph', repo, roots: [root], first_parent: false, filters });
  assert.deepEqual(await client.graph(repo, false, filters, ['abc']), { method: 'graph', repo, roots: [root], first_parent: false, filters, collapsed_segments: ['abc'] });
  assert.deepEqual(await client.graphRevision(repo), { method: 'graph_revision', repo, roots: [root] });
  assert.deepEqual(await client.theme(), { method: 'theme', roots: [root] });
  assert.deepEqual(await client.commitFiles(repo, 'abc123'), { method: 'commit_files', repo, roots: [root], oid: 'abc123' });
  assert.deepEqual(await client.branchReview(repo), { method: 'branch_review', repo, roots: [root] });
  assert.deepEqual(await client.inspectSubmodule(repo, 'child'), { method: 'submodule_inspect', repo, path: 'child', roots: [root] });
  assert.deepEqual(await client.inspectSubmodule(repo, 'child', true),
    { method: 'submodule_inspect', repo, path: 'child', roots: [root], summary_only: true });
  assert.deepEqual(await client.fileActionPreview(repo, 'file.txt', 'discard'), { method: 'file_action_preview', repo, roots: [root], path: 'file.txt', operation: 'discard' });
  assert.deepEqual(await client.planWorktree(repo, 'feature/a'), { method: 'worktree_plan', repo, roots: [root], branch: 'feature/a' });
});

test('an older backend requires an update for compact status', async t => {
  const client = fixture('old');
  t.after(() => client.dispose());
  const repo = path.resolve('repo');
  await client.initialize([path.resolve('workspace')]);
  for (const request of [() => client.status(repo, false, { summaryOnly: true, watchedPaths: [] }),
    () => client.inspectSubmodule(repo, 'child', true)]) {
    await assert.rejects(request(), error => error.code === 'unsupported_backend' && /update.*backend/i.test(error.message));
  }
  assert.equal((await client.status(repo)).method, 'status');
});

test('compact status fits the response limit when a full status does not', async t => {
  const client = fixture('large_status', { maxResponseBytes: 4096 });
  t.after(() => client.dispose());
  const repo = path.resolve('repo');
  await client.initialize([path.resolve('workspace')]);
  const result = await client.status(repo, false, { summaryOnly: true, watchedPaths: ['link'] });
  assert.deepEqual(result.watched_paths, ['link']);
  await assert.rejects(client.status(repo), error => error.code === 'response_too_large');
});

test('workspace updates change authorization before subsequent requests', async t => {
  const client = fixture('normal');
  t.after(() => client.dispose());
  const oldRoot = path.resolve('old-root');
  const newRoot = path.resolve('new-root');
  await client.initialize([oldRoot]);
  await client.initialize([newRoot]);
  assert.deepEqual((await client.status(path.join(newRoot, 'repo'))).roots, [newRoot]);
  await assert.rejects(client.status('/denied'), error => error.code === 'outside_workspace');
  assert.deepEqual((await client.status(path.join(newRoot, 'repo'))).roots, [newRoot]);
});

test('a request waiting on obsolete workspace initialization is rejected', async t => {
  const client = fixture('normal');
  t.after(() => client.dispose());
  const oldInitialization = client.initialize([path.resolve('old-root')]);
  const stale = client.status(path.resolve('old-root/repo'));
  const newInitialization = client.initialize([path.resolve('new-root')]);
  await oldInitialization;
  await assert.rejects(stale, error => error.code === 'workspace_changed');
  await newInitialization;
  assert.deepEqual((await client.status(path.resolve('new-root/repo'))).roots, [path.resolve('new-root')]);
});

test('unsupported protocol version stops initialization', async t => {
  const client = fixture('version');
  t.after(() => client.dispose());
  await assert.rejects(client.initialize([]), error => error.code === 'unsupported_version');
  await assert.rejects(client.status(path.resolve('repo')), error => error.code === 'not_initialized');
});

for (const [mode, code] of [['malformed', 'protocol_error'], ['invalid_utf8', 'protocol_error'], ['oversized', 'response_too_large']]) {
  test(`invalid service output fails explicitly: ${mode}`, async t => {
    const client = fixture(mode, { maxResponseBytes: 256 });
    t.after(() => client.dispose());
    await client.initialize([]);
    await assert.rejects(client.status(path.resolve('repo')), error => error.code === code);
  });
}

test('crashed service fails pending requests and requires reinitialization', async t => {
  const client = fixture('crash');
  t.after(() => client.dispose());
  await client.initialize([]);
  await assert.rejects(client.status(path.resolve('repo')), error => error.code === 'service_exited' || error.code === 'io_error');
  await assert.rejects(client.graph(path.resolve('repo')), error => error.code === 'not_initialized');
});

test('timed out replies cannot be mistaken for later results', async t => {
  const client = fixture('late');
  t.after(() => client.dispose());
  await client.initialize([]);
  client.timeoutMs = 100;
  await assert.rejects(client.status(path.resolve('repo')), error => error.code === 'timeout');
  client.timeoutMs = 30000;
  assert.equal((await client.graph(path.resolve('repo'))).method, 'graph');
});

test('disposing rejects pending work and prevents further requests', async () => {
  const client = fixture('never');
  await client.initialize([]);
  const pending = client.status(path.resolve('repo'));
  await new Promise(resolve => setImmediate(resolve));
  client.dispose();
  await assert.rejects(pending, error => error.code === 'disposed');
  await assert.rejects(client.status(path.resolve('repo')), error => error.code === 'disposed');
});
