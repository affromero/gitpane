'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { WorktreeDisplay } = require('../src/worktree-display');

const tick = () => new Promise(resolve => setImmediate(resolve));

function fixture() {
  const listeners = new Set();
  const requests = [];
  const backend = {
    generation: 1,
    state: { status: 'initializing' },
    onDidChange(handler) { listeners.add(handler); return { dispose: () => listeners.delete(handler) }; },
    uiConfig() { return new Promise((resolve, reject) => requests.push({ resolve, reject })); },
    emit(status) { this.state = { status }; for (const listener of listeners) listener(this.state); }
  };
  const display = new WorktreeDisplay(backend);
  return { backend, display, requests };
}

test('worktree display loads native settings and ignores stale workspaces', async () => {
  const { backend, display, requests } = fixture();
  try {
    backend.emit('ready');
    assert.equal(requests.length, 1);
    backend.generation++;
    backend.emit('initializing');
    backend.emit('ready');
    requests[0].resolve({ expand_worktrees: true });
    await tick();
    assert.equal(display.value, undefined);
    requests[1].resolve({ expand_worktrees: false, launcher_keys: ['w', 'j'], review_configured: true });
    await tick();
    assert.equal(display.value, false);
    assert.equal(display.error, undefined);
    assert.deepEqual(display.launcherKeys, ['w', 'j']);
    assert.equal(display.reviewConfigured, true);
    backend.emit('initializing');
    assert.deepEqual(display.launcherKeys, []);
    assert.equal(display.reviewConfigured, false);
  } finally { display.dispose(); }
});

test('invalid worktree settings are visible and can be retried', async () => {
  const { backend, display, requests } = fixture();
  try {
    backend.emit('ready');
    requests[0].resolve({ expand_worktrees: 'yes' });
    await tick();
    assert.match(display.error, /invalid worktree display/i);
    assert.equal(display.value, undefined);
    const retry = display.retry();
    requests[1].resolve({ expand_worktrees: true });
    await retry;
    assert.equal(display.value, true);
    assert.equal(display.error, undefined);
  } finally { display.dispose(); }
});

test('disposing ignores a late worktree settings response', async () => {
  const { backend, display, requests } = fixture();
  backend.emit('ready');
  display.dispose();
  requests[0].resolve({ expand_worktrees: true });
  await tick();
  assert.equal(display.value, undefined);
});
