'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { GithubProvider } = require('../src/github-provider');

function fixture() {
  const root = path.resolve('workspace');
  const repoPath = path.join(root, 'repo');
  const repo = { rootUri: { fsPath: repoPath, toString: () => `file:${repoPath}` } };
  let incarnation = 1;
  const tree = { git: { repositories: [repo] }, isVisible: () => true,
    repositoryToken: () => incarnation, isCurrentRepository: (item, token) => token === incarnation && tree.isVisible(item) };
  let generation = 1;
  let picks = [];
  let response = () => { throw new Error('Unexpected request'); };
  const calls = [];
  const documents = [];
  const errors = [];
  const menus = [];
  const clients = [];
  const backend = {
    mutationSnapshot: async () => ({ executable: path.join(root, 'gitpane'), roots: [root], generation }),
    isCurrentMutation: (snapshot, candidate) => snapshot.generation === generation && candidate === repoPath
  };
  const vscode = {
    Uri: { parse: value => value },
    env: { openExternal: async value => { calls.push(['browser', value]); } },
    workspace: { openTextDocument: async options => { documents.push(options); return options; } },
    window: {
      showQuickPick: async choices => {
        menus.push(choices);
        const pick = picks.shift();
        return pick ? pick(choices) : undefined;
      },
      showTextDocument: async document => { calls.push(['document', document]); },
      showErrorMessage: async message => { errors.push(message); }
    }
  };
  const provider = new GithubProvider(tree, backend, {
    vscode, realpath: async value => value,
    createClient: executable => {
      const client = {
        disposed: false,
        async initialize(roots) { calls.push(['initialize', executable, roots]); return { version: 1 }; },
        call(method, params) { calls.push(['request', method, params]); return response(method, params); },
        dispose() { this.disposed = true; }
      };
      clients.push(client);
      return client;
    }
  });
  return { provider, repo, repoPath, tree, calls, documents, errors, menus, clients,
    setPicks(values) { picks = values; }, setResponse(fn) { response = fn; },
    changeWorkspace() { generation++; }, reopen() { incarnation++; }
  };
}

function list(overrides = {}) {
  return { owner: 'acme', repo: 'project', issues: [], prs: [], issues_error: null, prs_error: null, limit: 50, ...overrides };
}

function item(number, title = 'Title') {
  return { number, title, state: 'OPEN', author: 'alice', updated_at: '2026-01-01', draft: false, checks: null };
}

function detail(overrides = {}) {
  return { owner: 'acme', repo: 'project', kind: 'pr', number: 7, title: 'A PR', author: 'alice', body: '<img src=x>',
    comments: [{ author: 'bob', body: 'Looks good', created_at: '2026-01-01' }], diff: null, diff_error: 'Diff unavailable', ...overrides };
}

const choose = (key, value) => choices => choices.find(item => item[key] === value);

test('on-demand GitHub list shows partial failures and opens only a constructed item URL', async () => {
  const f = fixture();
  f.setResponse(async method => method === 'github_list'
    ? list({ issues_error: 'Issues unavailable', prs: [{ ...item(7, 'PR $(check)'), checks: 'failure', url: 'https://evil.invalid' }] })
    : detail());
  f.setPicks([choose('number', 7), choose('action', 'browser')]);
  await f.provider.open(f.repo);
  assert.equal(f.calls.filter(call => call[0] === 'request').length, 2);
  assert.ok(f.menus[0].some(choice => choice.label === 'Issues unavailable'));
  assert.ok(f.menus[0].some(choice => choice.label.includes('＄(check)') && choice.description.includes('checks: failure')));
  assert.equal(f.documents[0].language, 'plaintext');
  assert.match(f.documents[0].content, /<img src=x>/);
  assert.match(f.documents[0].content, /bob.*2026-01-01[\s\S]*Looks good/);
  assert.match(f.documents[0].content, /Pull request diff unavailable: Diff unavailable/);
  assert.ok(f.calls.some(call => call[0] === 'browser' && call[1] === 'https://github.com/acme/project/pull/7'));
  assert.ok(f.clients.every(client => client.disposed));
});

test('filter and refresh fetch only when selected', async () => {
  const f = fixture();
  f.setResponse(async () => list());
  f.setPicks([choose('action', 'filter'), choose('value', 'closed'), choose('action', 'refresh')]);
  await f.provider.open(f.repo);
  const requests = f.calls.filter(call => call[0] === 'request');
  assert.deepEqual(requests.map(call => call[2].state), ['open', 'closed', 'closed']);
  assert.equal(f.clients.length, 3);
});

test('forged selections and invalid remote identities do not open URLs', async () => {
  const f = fixture();
  f.setResponse(async () => list({ prs: [item(7)] }));
  f.setPicks([() => ({ action: 'item', kind: 'pr', number: 123 })]);
  await f.provider.open(f.repo);
  assert.equal(f.calls.filter(call => call[0] === 'request').length, 1);
  assert.equal(f.calls.some(call => call[0] === 'browser'), false);
  const bad = fixture();
  bad.setResponse(async () => list({ owner: 'evil/other', prs: [item(7)] }));
  await bad.provider.open(bad.repo);
  assert.match(bad.errors[0], /invalid GitHub list/);
  assert.equal(bad.calls.some(call => call[0] === 'browser'), false);
});

test('workspace changes ignore an in-flight response without stopping its service', async () => {
  const f = fixture();
  let release;
  let requested;
  const started = new Promise(resolve => { requested = resolve; });
  f.setResponse(() => { requested(); return new Promise(resolve => { release = resolve; }); });
  const pending = f.provider.open(f.repo);
  await started;
  f.changeWorkspace();
  f.tree.git.repositories = [];
  f.provider.dispose();
  assert.equal(f.clients[0].disposed, false);
  release(list());
  await pending;
  assert.equal(f.menus.length, 0);
  assert.equal(f.documents.length, 0);
  assert.equal(f.clients[0].disposed, true);
});

test('a removed worktree link during selection prevents opening GitHub details', async () => {
  const f = fixture();
  let linked = true;
  f.setResponse(async () => list({ prs: [item(7)] }));
  f.setPicks([choices => { linked = false; return choices.find(choice => choice.number === 7); }]);
  await f.provider.open(f.repo, () => linked);
  assert.equal(f.calls.filter(call => call[0] === 'request').length, 1);
  assert.deepEqual(f.documents, []);
  assert.equal(f.calls.some(call => call[0] === 'browser'), false);
});

test('reopening the same repository path during selection prevents opening GitHub details', async () => {
  const f = fixture();
  f.setResponse(async () => list({ prs: [item(7)] }));
  f.setPicks([choices => { f.reopen(); return choices.find(choice => choice.number === 7); }]);
  await f.provider.open(f.repo);
  assert.deepEqual(f.documents, []);
  assert.equal(f.calls.filter(call => call[0] === 'request').length, 1);
});

test('item detail must match the repository and number in the selected list', async () => {
  const f = fixture();
  f.setResponse(async method => method === 'github_list' ? list({ prs: [item(7)] }) : detail({ repo: 'other' }));
  f.setPicks([choose('number', 7)]);
  await f.provider.open(f.repo);
  assert.match(f.errors[0], /invalid GitHub detail/);
  assert.equal(f.documents.length, 0);
  assert.equal(f.calls.some(call => call[0] === 'browser'), false);
});
