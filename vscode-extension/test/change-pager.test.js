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

test('search navigates matches on other pages and refreshes after the snapshot changes', async () => {
  let revision = 'a'.repeat(64), snapshot = 0, matches = ['one', 'two'], failSearch = false;
  const backend = {
    async beginChanges() { return page(`s${++snapshot}`, revision, 0, null, 1, ['one']); },
    async nextChanges(_repo, id, cursor) {
      return cursor === 0 ? page(id, revision, 0, null, 1, ['one']) : page(id, revision, cursor, 0, null, ['two']);
    },
    async searchChanges(_repo, id, _query, direction, position) {
      if (failSearch) throw Object.assign(new Error('expired'), { code: 'snapshot_stale' });
      const index = matches.length ? direction === 'next' ? ((position ?? -1) + 1) % matches.length : 0 : null;
      const selected = index === null ? null : matches[index];
      return { snapshot: id, revision, count: matches.length, position: index,
        cursor: selected === null ? null : selected === 'one' ? 0 : 1,
        page: selected === null ? null : selected === 'one' ? 0 : 1,
        path: selected, group: selected === null ? null : 'untracked' };
    },
    async releaseChanges() {}
  };
  const pager = new ChangePager(backend, () => {});
  await pager.refresh('owner', '/repo', () => true);
  await pager.search('owner', 'o');
  assert.equal(pager.get('owner').search.path, 'one');
  await pager.search('owner', 'o', 'next');
  assert.equal(pager.get('owner').search.path, 'two');
  assert.equal(pager.get('owner').changes[0].path, 'two');
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').search.path, 'two');
  assert.equal(pager.get('owner').search.position, 1);
  failSearch = true;
  await pager.search('owner', 'o', 'next');
  assert.equal(pager.get('owner').search.phase, 'error');
  failSearch = false;
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').search.phase, 'ready');
  assert.equal(pager.get('owner').search.path, 'one');
  matches = [];
  revision = 'b'.repeat(64);
  await pager.refresh('owner', '/repo', () => true);
  assert.equal(pager.get('owner').search.count, 0);
  assert.equal(pager.get('owner').search.position, null);
  pager.dispose();
});

test('search rejects late queries and removed repositories and reports failed refreshes', async () => {
  let release, alive = true, failRefresh = false;
  const backend = {
    async beginChanges() { if (failRefresh) throw new Error('scan failed'); return page('s', 'a'.repeat(64), 0, null, null, ['one']); },
    searchChanges(_repo, snapshot, query) { return new Promise(resolve => { release = () => resolve({ snapshot, revision: 'a'.repeat(64),
      count: 1, position: 0, cursor: 0, page: 0, path: 'one', group: 'untracked', query }); }); },
    async releaseChanges() {}
  };
  const pager = new ChangePager(backend, () => {});
  await pager.refresh('owner', '/repo', () => alive);
  const old = pager.search('owner', 'old');
  const latest = pager.search('owner', 'new');
  release();
  await old;
  await new Promise(resolve => setImmediate(resolve));
  release();
  await latest;
  assert.equal(pager.get('owner').search.query, 'new');
  assert.equal(pager.get('owner').search.phase, 'ready');
  const pending = pager.search('owner', 'another');
  alive = false;
  release();
  await pending;
  assert.notEqual(pager.get('owner').search.phase, 'ready');
  alive = true;
  failRefresh = true;
  await pager.refresh('owner', '/repo', () => alive);
  await pager.search('owner', 'one');
  assert.equal(pager.get('owner').search.phase, 'error');
  assert.match(pager.get('owner').search.error, /scan failed/);
  pager.dispose();
});
