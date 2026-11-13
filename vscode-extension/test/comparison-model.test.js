'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ComparisonModel } = require('../src/comparison-model');

test('comparison remains pinned until refresh and preserves direction and mode', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), moved = 'c'.repeat(40), identity = 'd'.repeat(64);
  const refs = { main: first, feature: second };
  const calls = [];
  const service = { cancel() {}, async request(method, _repo, params) {
    calls.push({ method, ...params });
    if (method === 'resolve_revision') return { oid: refs[params.revision], repo_identity: identity };
    return { base_oid: params.from, to_oid: params.to, repo_identity: identity, total: 1, next: null,
      files: [{ path: 'file.txt', status: 'M', is_submodule: false }] };
  } };
  const model = new ComparisonModel(service, '/repo', () => true);
  await model.set('from', 'main');
  await model.set('to', 'feature');
  refs.main = moved;
  await model.checkRefs();
  assert.equal(model.from.moved, true);
  assert.equal(model.from.oid, first);
  await model.compare();
  assert.equal(calls.at(-1).from, first);
  model.swap();
  model.setMode('common_ancestor');
  await model.compare();
  assert.deepEqual({ from: calls.at(-1).from, to: calls.at(-1).to, mode: calls.at(-1).common_ancestor },
    { from: second, to: first, mode: true });
  await model.refresh();
  await model.compare();
  assert.equal(calls.at(-1).to, moved);
  model.clear();
  await assert.rejects(model.compare(), /both comparison endpoints/i);
});

test('comparison includes every page and rejects an inconsistent page', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'd'.repeat(64);
  let inconsistent = false;
  const service = { cancel() {}, async request(method, _repo, params) {
    if (method === 'resolve_revision') return { oid: params.revision, repo_identity: identity };
    const count = params.offset === 0 ? 256 : 1;
    return { base_oid: params.offset && inconsistent ? second : first, to_oid: second, repo_identity: identity,
      total: 257, next: params.offset === 0 ? 256 : null,
      files: Array.from({ length: count }, (_, index) => ({ status: 'M', path: `file-${params.offset + index}`, is_submodule: false })) };
  } };
  const model = new ComparisonModel(service, '/repo', () => true);
  await model.set('from', first); await model.set('to', second);
  const result = await model.compare();
  assert.equal(result.files.length, 257);
  assert.equal(result.files[256].path, 'file-256');
  inconsistent = true;
  await assert.rejects(model.compare(), /invalid or stale/i);
});
