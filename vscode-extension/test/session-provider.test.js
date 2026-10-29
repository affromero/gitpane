'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { SessionProvider } = require('../src/session-provider');

function fixture() {
  const root = path.resolve('workspace/repo');
  const repo = { rootUri: { fsPath: root, toString: () => root } };
  let visible = true;
  let sessions = ['main'];
  let listError;
  let gotoError;
  let duringList = () => {};
  let beforeDispatch = () => {};
  let afterDispatch = () => {};
  const picks = [];
  const requests = [];
  const navigation = [];
  const errors = [];
  const information = [];
  const tree = { git: { repositories: [repo] }, isVisible: () => visible };
  const vscode = { window: {
    async showQuickPick(choices) {
      const choose = picks.shift();
      assert.ok(choose, 'unexpected picker');
      return choose(choices);
    },
    async showInformationMessage(message) { information.push(message); },
    async showErrorMessage(message) { errors.push(message); }
  } };
  const backend = {
    async listSessions(repoPath, kind) {
      requests.push({ repoPath, kind });
      await duringList();
      if (listError) throw listError;
      return { sessions };
    },
    async gotoSession(repoPath, kind, session, validateTarget) {
      await beforeDispatch();
      if (!validateTarget()) throw new Error('Repository or workspace changed before session navigation.');
      if (gotoError) throw gotoError;
      navigation.push({ repoPath, kind, session });
      await afterDispatch();
      return {};
    }
  };
  const provider = new SessionProvider(tree, backend, { vscode });
  return {
    root, repo, tree, provider, picks, requests, navigation, errors, information,
    setVisible(value) { visible = value; },
    setSessions(value) { sessions = value; },
    setListError(value) { listError = value; },
    setGotoError(value) { gotoError = value; },
    onList(callback) { duringList = callback; },
    beforeDispatch(callback) { beforeDispatch = callback; },
    afterDispatch(callback) { afterDispatch = callback; }
  };
}

test('opens the selected in-workspace session using its exact identifier', async () => {
  const f = fixture();
  f.setSessions(['main', 'topic']);
  f.picks.push(choices => choices.find(choice => choice.kind === 'tmux'));
  f.picks.push(choices => choices.find(choice => choice.session === 'topic'));
  await f.provider.open(f.repo);
  assert.deepEqual(f.requests, [{ repoPath: f.root, kind: 'tmux' }]);
  assert.deepEqual(f.navigation, [{ repoPath: f.root, kind: 'tmux', session: 'topic' }]);
  assert.deepEqual(f.errors, []);
});

test('sanitizes session labels while preserving the exact herdr identifier', async () => {
  const f = fixture();
  const identifier = 'work\n$(warning)\u202e';
  f.setSessions([identifier]);
  f.picks.push(choices => choices.find(choice => choice.kind === 'herdr'));
  f.picks.push(choices => {
    assert.equal(choices[0].label.includes('\n'), false);
    assert.equal(choices[0].label.includes('$('), false);
    assert.equal(choices[0].label.includes('\u202e'), false);
    return choices[0];
  });
  await f.provider.open(f.repo);
  assert.deepEqual(f.navigation, [{ repoPath: f.root, kind: 'herdr', session: identifier }]);
});

test('cancellation and forged picker values never open a session', async () => {
  const f = fixture();
  f.picks.push(() => undefined);
  await f.provider.open(f.repo);
  f.picks.push(choices => choices[0]);
  f.picks.push(() => ({ label: 'forged', session: 'outside' }));
  await f.provider.open(f.repo);
  assert.deepEqual(f.navigation, []);
  assert.deepEqual(f.errors, []);
});

test('workspace removal during either picker or session lookup stops navigation', async () => {
  const f = fixture();
  f.picks.push(choices => { f.setVisible(false); return choices[0]; });
  await f.provider.open(f.repo);
  f.setVisible(true);
  f.picks.push(choices => choices[0]);
  f.onList(() => f.setVisible(false));
  await f.provider.open(f.repo);
  f.setVisible(true);
  f.onList(() => {});
  f.picks.push(choices => choices[0]);
  f.picks.push(choices => { f.tree.git.repositories = []; return choices[0]; });
  await f.provider.open(f.repo);
  assert.deepEqual(f.navigation, []);
  assert.equal(f.errors.length, 3);
  assert.ok(f.errors.every(message => /workspace changed/i.test(message)));
});

test('disposing during a picker prevents a pending navigation', async () => {
  const f = fixture();
  let finish;
  f.picks.push(choices => new Promise(resolve => { finish = () => resolve(choices[0]); }));
  const pending = f.provider.open(f.repo);
  f.provider.dispose();
  finish();
  await pending;
  assert.deepEqual(f.navigation, []);
  assert.match(f.errors[0], /workspace changed/i);
});

test('a workspace change during service initialization prevents dispatch', async () => {
  const f = fixture();
  f.picks.push(choices => choices[0]);
  f.picks.push(choices => choices[0]);
  f.beforeDispatch(() => f.setVisible(false));
  await f.provider.open(f.repo);
  assert.deepEqual(f.navigation, []);
  assert.match(f.errors[0], /workspace changed/i);
});

test('a successful dispatched navigation remains successful after a workspace change', async () => {
  const f = fixture();
  let finish;
  f.picks.push(choices => choices[0]);
  f.picks.push(choices => choices[0]);
  f.afterDispatch(() => new Promise(resolve => { finish = resolve; }));
  const pending = f.provider.open(f.repo);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(f.navigation, [{ repoPath: f.root, kind: 'tmux', session: 'main' }]);
  f.setVisible(false);
  finish();
  await pending;
  assert.deepEqual(f.errors, []);
});

test('missing sessions, malformed responses and backend errors remain visible', async () => {
  const f = fixture();
  f.setSessions([]);
  f.picks.push(choices => choices[0]);
  await f.provider.open(f.repo);
  assert.match(f.information[0], /no tmux sessions/i);
  f.setSessions(['ok', null]);
  f.picks.push(choices => choices[0]);
  await f.provider.open(f.repo);
  assert.match(f.errors[0], /invalid session list/i);
  f.setListError(new Error('tmux is unavailable'));
  f.picks.push(choices => choices[0]);
  await f.provider.open(f.repo);
  assert.match(f.errors[1], /tmux is unavailable/i);
  f.setListError(undefined);
  f.setSessions(['main']);
  f.setGotoError(new Error('session ended'));
  f.picks.push(choices => choices[0]);
  f.picks.push(choices => choices[0]);
  await f.provider.open(f.repo);
  assert.match(f.errors[2], /session ended/i);
  assert.deepEqual(f.navigation, []);
});
