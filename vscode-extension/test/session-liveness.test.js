'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { SessionLiveness } = require('../src/session-liveness');

async function waitFor(predicate) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('Session liveness did not settle');
    await new Promise(resolve => setTimeout(resolve, 5));
  }
}

function fixture() {
  let targets = [{ path: '/workspace/main', references: ['main:1'] }, { path: '/workspace/topic', references: ['topic:1'] }];
  let request = async paths => ({ enabled: true, reason: null, repos: paths.map(repo => ({ repo, live: repo.endsWith('/topic') })) });
  let changes = 0, calls = 0, cancelled = 0;
  const tree = { sessionTargets: () => targets, changed: () => { changes++; controller.changed(); } };
  const backend = { generation: 1, sessionLiveness: paths => { calls++; return request(paths); },
    cancelSessionLiveness: () => { cancelled++; } };
  const controller = new SessionLiveness(tree, backend);
  return { controller, backend, get changes() { return changes; }, get calls() { return calls; }, get cancelled() { return cancelled; },
    targets(value) { targets = value; controller.changed(); }, request(value) { request = value; } };
}

test('visible session probes distinguish independent worktrees and honor disabled hosts', async t => {
  const f = fixture();
  t.after(() => f.controller.dispose());
  assert.equal(f.calls, 0);
  f.controller.setVisible(true);
  await waitFor(() => f.changes > 0);
  assert.equal(f.controller.live.get('/workspace/main'), false);
  assert.equal(f.controller.live.get('/workspace/topic'), true);
  assert.equal(f.controller.error, undefined);
  f.request(async () => ({ enabled: false, reason: 'unsupported', repos: [] }));
  f.targets([{ path: '/workspace/main', references: ['main:2'] }]);
  await waitFor(() => f.calls >= 2 && !f.controller.pending);
  assert.equal(f.controller.live.size, 0);
  assert.equal(f.controller.error, undefined);
  f.controller.setVisible(false);
  const count = f.calls;
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(f.calls, count);
  assert.ok(f.cancelled > 0);
});

test('stale workspace and hidden-view responses never publish session indicators', async t => {
  const f = fixture();
  t.after(() => f.controller.dispose());
  let release;
  f.request(() => new Promise(resolve => { release = resolve; }));
  f.controller.setVisible(true);
  await waitFor(() => !!release);
  f.targets([{ path: '/workspace/main', references: ['main:reopened'] }]);
  f.controller.setVisible(false);
  release({ enabled: true, reason: null, repos: [
    { repo: '/workspace/main', live: true }, { repo: '/workspace/topic', live: true }
  ] });
  await waitFor(() => !f.controller.pending);
  assert.equal(f.controller.live.size, 0);
  assert.equal(f.changes, 0);
  f.controller.dispose();
  f.controller.setVisible(true);
  assert.equal(f.calls, 1);
});

test('session probe errors remain visible and recover without changing repository status', async t => {
  const f = fixture();
  t.after(() => f.controller.dispose());
  f.request(async () => { throw new Error('tmux unavailable'); });
  f.controller.setVisible(true);
  await waitFor(() => !!f.controller.error);
  assert.match(f.controller.error, /tmux unavailable/);
  f.request(async paths => ({ enabled: true, reason: null, repos: paths.map(repo => ({ repo, live: true })) }));
  f.targets([{ path: '/workspace/main', references: ['main:2'] }]);
  await waitFor(() => f.controller.live.get('/workspace/main') === true);
  assert.equal(f.controller.error, undefined);
});

test('changing the session host waits for an in-flight probe and then refreshes', async t => {
  const f = fixture();
  t.after(() => f.controller.dispose());
  let release;
  f.request(() => new Promise(resolve => { release = resolve; }));
  f.controller.setVisible(true);
  await waitFor(() => !!release);
  f.request(async paths => ({ enabled: true, reason: null, repos: paths.map(repo => ({ repo, live: true })) }));
  f.controller.restart();
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(f.calls, 1, 'setting changes do not start another probe while the first is running');
  release({ enabled: true, reason: null, repos: [
    { repo: '/workspace/main', live: false }, { repo: '/workspace/topic', live: false }
  ] });
  await waitFor(() => f.calls === 2 && f.controller.live.get('/workspace/main') === true);
  assert.equal(f.controller.error, undefined);
});
