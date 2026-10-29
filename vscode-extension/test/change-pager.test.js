'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ChangePager, validRecord } = require('../src/change-pager');

const record = path => ({ path, group: 'untracked', kind: 'untracked', original_path: null,
  conflict_stages: null, is_submodule: false });
const page = (snapshot, revision, cursor, previous, next, files) => ({ snapshot, revision, cursor,
  previous_cursor: previous, next_cursor: next, changes: files.map(record) });

test('attention records are accepted only for a scoped submodule', () => {
  const attention = { group: 'attention', kind: 'submodule_attention', path: 'vendor/child',
    original_path: null, conflict_stages: null, is_submodule: true };
  assert.equal(validRecord(attention), true);
  assert.equal(validRecord({ ...attention, is_submodule: false }), false);
  assert.equal(validRecord({ ...attention, path: '../outside' }), false);
  assert.equal(validRecord({ ...attention, original_path: 'other' }), false);
});

test('paging preserves the current position across unchanged refreshes and resets after changes', async () => {
  let sequence = 0;
  let revision = 'a'.repeat(64);
  const backend = {
    async beginChanges() { return page(`s${++sequence}`, revision, 0, null, 1, ['one']); },
    async nextChanges(_repo, snapshot, cursor) { return page(snapshot, revision, cursor, 0, null, ['two']); },
    async releaseChanges() {}
  };
  const pager = new ChangePager(backend, () => {});
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').changes[0].path, 'one');
  await pager.move('owner', 'next');
  assert.equal(pager.get('owner').changes[0].path, 'two');
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').changes[0].path, 'two');
  assert.equal(pager.get('owner').page, 1);
  revision = 'b'.repeat(64);
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').changes[0].path, 'one');
  assert.equal(pager.get('owner').page, 0);
  pager.dispose();
});

test('a removed owner cannot publish a late page', async () => {
  let resolve;
  const backend = { beginChanges: () => new Promise(done => { resolve = done; }),
    async releaseChanges() {}, cancelChanges() {} };
  const pager = new ChangePager(backend, () => {});
  const pending = pager.refresh('owner', '/repo', () => true);
  await Promise.resolve();
  pager.release('owner');
  resolve(page('old', 'a'.repeat(64), 0, null, null, ['old']));
  await pending;
  assert.equal(pager.get('owner'), undefined);
  pager.dispose();
});

test('invalid and expired pages remain visible errors rather than partial changes', async () => {
  const backend = { async beginChanges() { return page('first', 'a'.repeat(64), 0, null, 1, ['file']); },
    async nextChanges() { const error = new Error('expired'); error.code = 'snapshot_stale'; throw error; },
    async releaseChanges() {} };
  const pager = new ChangePager(backend, () => {});
  await pager.refresh('owner', '/repo', () => true);
  await pager.move('owner', 'next');
  assert.equal(pager.get('owner').phase, 'error');
  assert.match(pager.get('owner').error, /expired/i);
  assert.deepEqual(pager.get('owner').changes, []);
  backend.beginChanges = async () => page('bad', 'a'.repeat(64), 0, null, null, ['../outside']);
  await pager.refresh('owner', '/repo', () => true);
  assert.match(pager.get('owner').error, /invalid changes page/i);
  pager.dispose();
});
