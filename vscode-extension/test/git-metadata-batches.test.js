'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { GitMetadataBatches } = require('../src/git-metadata-batches');
const { MAX_REQUEST_BYTES } = require('../src/bridge-client');

function session(repo = '/workspace/repo', key = 'git') {
  return { repo, git: { key, executable: '/usr/bin/git' }, snapshot: { executable: '/gitpane', generation: 1,
    roots: ['/workspace'], rootSources: ['/workspace'], rootIdentities: ['a'.repeat(64)] } };
}
function response(items) {
  return { version: 1, volatile: false, replacements: 'b'.repeat(64), config: 'c'.repeat(64),
    results: items.map(item => ({ metadata: { path: item.path } })) };
}
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

test('concurrent metadata requests preserve each result and separate repository and environment scopes', async t => {
  const calls = [];
  const batches = new GitMetadataBatches(async (owner, identity, items) => {
    calls.push({ owner, identity, items });
    return response(items);
  });
  t.after(() => batches.dispose());
  const owner = session(), other = session('/workspace/other'), changedEnv = session('/workspace/repo', 'other');
  const inputs = Array.from({ length: 100 }, (_, index) => ({ ref: 'HEAD', path: `file-${index}` }));
  const results = await Promise.all([...inputs.map(item => batches.request(owner, 'd'.repeat(64), item)),
    batches.request(other, 'd'.repeat(64), { ref: 'HEAD', path: 'other' }),
    batches.request(changedEnv, 'd'.repeat(64), { ref: 'HEAD', path: 'environment' })]);
  assert.deepEqual(results.map(value => value.results[0].metadata.path), [...inputs.map(item => item.path), 'other', 'environment']);
  assert.deepEqual(calls.map(call => call.items.length).sort((a, b) => a - b), [1, 1, 100]);
});

test('metadata batches split at protocol limits without poisoning valid items', async t => {
  const calls = [], owner = session(), identity = 'd'.repeat(64);
  const batches = new GitMetadataBatches(async (current, expected, items) => {
    const request = { version: 1, workspace_roots: current.snapshot.roots, workspace_root_identities: current.snapshot.rootIdentities,
      workspace_root_sources: current.snapshot.rootSources, repo: current.repo, expected_repo_identity: expected,
      git_executable: current.git.executable, operation: { kind: 'metadata', items } };
    assert.ok(Buffer.byteLength(`${JSON.stringify(request)}\n`) <= MAX_REQUEST_BYTES);
    assert.ok(items.length <= 128);
    calls.push(items);
    return response(items);
  });
  t.after(() => batches.dispose());
  const inputs = Array.from({ length: 130 }, (_, index) => ({ ref: ':0', path: `file-${index}` }));
  inputs.push(...Array.from({ length: 4 }, (_, index) => ({ ref: 'HEAD', path: `${index}-${'x'.repeat(300000)}` })));
  const results = await Promise.all([...inputs.map(item => batches.request(owner, identity, item)),
    batches.request(owner, identity, { ref: 'HEAD', path: '../invalid' }).catch(error => error),
    batches.request(owner, identity, { ref: 'HEAD', path: 'x'.repeat(MAX_REQUEST_BYTES) }).catch(error => error)]);
  assert.deepEqual(results.slice(0, inputs.length).map(value => value.results[0].metadata.path), inputs.map(item => item.path));
  assert.equal(results.at(-2).code, 'invalid_request');
  assert.equal(results.at(-1).code, 'request_too_large');
  assert.ok(calls.length >= 3);
});

test('canceling one subscriber preserves the other, and canceling all stops the worker', async t => {
  const started = deferred(), release = deferred();
  let workerSignal;
  const batches = new GitMetadataBatches(async (_owner, _identity, items, signal) => {
    workerSignal = signal;
    started.resolve();
    await release.promise;
    return response(items);
  });
  t.after(() => batches.dispose());
  const first = new AbortController(), second = new AbortController(), owner = session();
  const one = batches.request(owner, 'd'.repeat(64), { ref: 'HEAD', path: 'one' }, first.signal);
  const two = batches.request(owner, 'd'.repeat(64), { ref: 'HEAD', path: 'two' }, second.signal);
  const settled = Promise.allSettled([one, two]);
  await started.promise;
  first.abort();
  assert.equal(workerSignal.aborted, false);
  release.resolve();
  const results = await settled;
  assert.equal(results[0].reason.code, 'cancelled');
  assert.equal(results[1].value.results[0].metadata.path, 'two');

  const active = deferred();
  let sharedSignal;
  const pending = new GitMetadataBatches((_owner, _identity, _items, signal) => {
    sharedSignal = signal;
    active.resolve();
    return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true }));
  });
  t.after(() => pending.dispose());
  const third = new AbortController(), fourth = new AbortController();
  const both = Promise.allSettled([pending.request(owner, 'd'.repeat(64), { ref: 'HEAD', path: 'three' }, third.signal),
    pending.request(owner, 'd'.repeat(64), { ref: 'HEAD', path: 'four' }, fourth.signal)]);
  await active.promise;
  third.abort(); fourth.abort();
  assert.equal(sharedSignal.aborted, true);
  assert.deepEqual((await both).map(value => value.reason.code), ['cancelled', 'cancelled']);
});

test('completed metadata is not cached and disposal rejects queued callers', async () => {
  let value = 'before';
  const batches = new GitMetadataBatches(async (_owner, _identity, items) => response(items.map(item => ({ ...item, path: value }))));
  const owner = session(), item = { ref: 'HEAD', path: 'file' };
  assert.equal((await batches.request(owner, 'd'.repeat(64), item)).results[0].metadata.path, 'before');
  value = 'after';
  assert.equal((await batches.request(owner, 'd'.repeat(64), item)).results[0].metadata.path, 'after');
  const queued = batches.request(owner, 'd'.repeat(64), item);
  const rejected = assert.rejects(queued, error => error.code === 'cancelled');
  batches.dispose();
  await rejected;
  await assert.rejects(batches.request(owner, 'd'.repeat(64), item), error => error.code === 'disposed');
});

test('per-item errors stay isolated while worker failures reach every caller', async t => {
  const owner = session(), identity = 'd'.repeat(64);
  let broken = false;
  const batches = new GitMetadataBatches(async (_owner, _identity, items) => {
    if (broken) throw Object.assign(new Error('Git unavailable'), { code: 'repository_error' });
    return { ...response(items), results: items.map(item => item.path === 'missing' ?
      { error: { code: 'content_not_found', message: 'Missing' } } : { metadata: { path: item.path } }) };
  });
  t.after(() => batches.dispose());
  const [missing, present] = await Promise.all([batches.request(owner, identity, { ref: 'HEAD', path: 'missing' }),
    batches.request(owner, identity, { ref: 'HEAD', path: 'present' })]);
  assert.equal(missing.results[0].error.code, 'content_not_found');
  assert.equal(present.results[0].metadata.path, 'present');
  broken = true;
  const failures = await Promise.allSettled([batches.request(owner, identity, { ref: 'HEAD', path: 'one' }),
    batches.request(owner, identity, { ref: 'HEAD', path: 'two' })]);
  assert.deepEqual(failures.map(item => item.reason.code), ['repository_error', 'repository_error']);
});

test('reconfiguration cancels old metadata and allows a fresh workspace request', async t => {
  const started = deferred();
  let first = true, oldSignal;
  const batches = new GitMetadataBatches(async (_owner, _identity, items, signal) => {
    if (!first) return response(items);
    first = false;
    oldSignal = signal;
    started.resolve();
    return await new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true }));
  });
  t.after(() => batches.dispose());
  const owner = session(), identity = 'd'.repeat(64);
  const old = batches.request(owner, identity, { ref: 'HEAD', path: 'old' });
  const rejected = assert.rejects(old, error => error.code === 'cancelled');
  await started.promise;
  batches.cancel();
  await rejected;
  assert.equal(oldSignal.aborted, true);
  const next = session();
  next.snapshot.generation++;
  const result = await batches.request(next, identity, { ref: 'HEAD', path: 'new' });
  assert.equal(result.results[0].metadata.path, 'new');
});
