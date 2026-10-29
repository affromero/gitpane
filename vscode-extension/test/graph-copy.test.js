'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');

async function fixture(t) {
  const copies = [], errors = [], messages = [];
  let receive, disposePanel, choose = choices => choices[0], failCopy = false, onCommit;
  const panel = { visible: true, webview: { postMessage(message) { messages.push(message); }, onDidReceiveMessage(handler) { receive = handler; } },
    onDidDispose(handler) { disposePanel = handler; }, dispose() { disposePanel(); }, reveal() {} };
  const uri = value => ({ fsPath: value, toString: () => value });
  const vscode = { Uri: { file: uri, joinPath: (...parts) => parts.join('/') }, ViewColumn: { Active: 1 },
    window: { createWebviewPanel: () => panel, showQuickPick: choices => choose(choices),
      showErrorMessage: message => { errors.push(message); } },
    env: { clipboard: { async writeText(value) { if (failCopy) throw new Error('clipboard unavailable'); copies.push(value); } } } };
  const load = Module._load;
  let GraphProvider;
  delete require.cache[require.resolve('../src/graph-provider')];
  try {
    Module._load = function(name, ...args) {
      if (name === 'vscode') return vscode;
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    ({ GraphProvider } = require('../src/graph-provider'));
    const root = path.resolve('workspace/repo');
    const repo = { rootUri: uri(root) };
    let incarnation = 1;
    const tree = { git: { repositories: [repo] }, isVisible: () => true,
      repositoryToken: () => incarnation, isCurrentRepository: (_repo, token) => token === incarnation,
      onDidChangeTreeData: () => ({ dispose() {} }) };
    const oid = 'a'.repeat(40), next = 'b'.repeat(40);
    const backend = { async graphRevision() { return { revision: 'f'.repeat(64) }; },
      async graph() { return { branches: [], rows: [
        { oid, short_id: 'aaaaaaa', message: 'first subject' },
        { oid: next, short_id: 'bbbbbbb', message: 'second subject' }
      ] }; }, async commitFiles(_repo, selected) {
      if (onCommit) await onCommit(selected);
      return { files: [{ path: 'src/example.rs', status: 'M' }] };
    } };
    const provider = new GraphProvider(tree, backend, {}, '/extension');
    provider.open(repo);
    await receive({ type: 'ready' });
    await receive({ type: 'selectCommit', oid });
    t.after(() => { provider.dispose(); delete require.cache[require.resolve('../src/graph-provider')]; });
    return { provider, panel, receive, oid, next, copies, errors, messages,
      setChoice(value) { choose = value; }, setFailure(value) { failCopy = value; },
      setOnCommit(value) { onCommit = value; },
      replaceRepository() { incarnation++; } };
  } finally { Module._load = load; }
}

test('commit actions copy the selected backend hash and subject', async t => {
  const f = await fixture(t);
  for (const [index, expected] of [[0, f.oid], [1, 'aaaaaaa'], [2, 'first subject']]) {
    f.setChoice(choices => choices[index]);
    await f.receive({ type: 'commitActions', oid: f.oid });
    assert.equal(f.copies.at(-1), expected);
  }
  await f.receive({ type: 'copyCommitSummary', oid: f.oid });
  assert.equal(f.copies.at(-1), 'aaaaaaa first subject');
  assert.deepEqual(f.errors, []);
});

test('commit actions reject stale, fabricated, collapsed and unavailable rows', async t => {
  const f = await fixture(t);
  await f.receive({ type: 'commitActions', oid: f.next });
  await f.receive({ type: 'commitActions', oid: 'c'.repeat(40) });
  const row = f.provider.rows.get(f.oid);
  f.provider.rows.set(f.oid, { ...row, collapsed: { count: 3 } });
  await f.receive({ type: 'commitActions', oid: f.oid });
  f.provider.rows.set(f.oid, row);
  f.provider.interactive = false;
  await f.receive({ type: 'commitActions', oid: f.oid });
  f.provider.interactive = true;
  f.panel.visible = false;
  await f.receive({ type: 'commitActions', oid: f.oid });
  f.panel.visible = true;
  f.replaceRepository();
  await f.receive({ type: 'commitActions', oid: f.oid });
  assert.deepEqual(f.copies, []);
});

test('commit action picker cannot copy after selection or graph changes', async t => {
  const f = await fixture(t);
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setChoice(choices => new Promise(resolve => { finish = () => resolve(choices[0]); started(); }));
  const pending = f.receive({ type: 'commitActions', oid: f.oid });
  await began;
  await f.receive({ type: 'selectCommit', oid: f.next });
  finish();
  await pending;
  assert.deepEqual(f.copies, []);
  await f.receive({ type: 'selectCommit', oid: f.oid });
  let resume, pickerOpened;
  const picker = new Promise(resolve => { pickerOpened = resolve; });
  f.setChoice(choices => new Promise(resolve => { resume = () => resolve(choices[0]); pickerOpened(); }));
  const oldGraph = f.receive({ type: 'commitActions', oid: f.oid });
  await picker;
  await f.receive({ type: 'refresh' });
  resume();
  await oldGraph;
  assert.deepEqual(f.copies, []);
});

test('selecting a collapsed branch cancels an open commit action', async t => {
  const f = await fixture(t);
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setChoice(choices => new Promise(resolve => { finish = () => resolve(choices[0]); started(); }));
  const pending = f.receive({ type: 'commitActions', oid: f.oid });
  await began;
  const row = f.provider.rows.get(f.next);
  f.provider.rows.set(f.next, { ...row, collapsed: { id: f.next, count: 2 } });
  await f.receive({ type: 'selectCollapsed', oid: f.next });
  finish();
  await pending;
  assert.deepEqual(f.copies, []);
});

test('selecting a collapsed branch cancels a commit still being validated', async t => {
  const f = await fixture(t);
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  f.provider.validateGuard = () => new Promise(resolve => { finish = () => resolve(true); started(); });
  const pending = f.receive({ type: 'selectCommit', oid: f.next });
  await began;
  const row = f.provider.rows.get(f.oid);
  f.provider.rows.set(f.oid, { ...row, collapsed: { id: f.oid, count: 2 } });
  await f.receive({ type: 'selectCollapsed', oid: f.oid });
  finish();
  await pending;
  assert.equal(f.provider.selected, undefined);
  assert.equal(f.provider.files.size, 0);
  await f.receive({ type: 'copyCommitSummary', oid: f.next });
  assert.deepEqual(f.copies, []);
});

test('a new pending selection immediately invalidates the previous commit and files', async t => {
  const f = await fixture(t);
  const oldFile = f.messages.at(-1).files[0];
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  const validation = new Promise(resolve => { finish = () => resolve(true); });
  f.provider.validateGuard = () => { started(); return validation; };
  const pending = f.receive({ type: 'selectCommit', oid: f.next });
  await began;
  await f.receive({ type: 'copyCommitSummary', oid: f.oid });
  await f.receive({ type: 'copyCommitFile', id: oldFile.id });
  assert.deepEqual(f.copies, []);
  finish();
  await pending;
  await f.receive({ type: 'copyCommitSummary', oid: f.next });
  assert.deepEqual(f.copies, ['bbbbbbb second subject']);
});

test('a fabricated commit selection leaves the current commit available', async t => {
  const f = await fixture(t);
  await f.receive({ type: 'selectCommit', oid: 'c'.repeat(40) });
  await f.receive({ type: 'copyCommitSummary', oid: f.oid });
  assert.deepEqual(f.copies, ['aaaaaaa first subject']);
});

test('a reloaded graph accepts selection and copying while restored details load', async t => {
  const f = await fixture(t);
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setOnCommit(async oid => {
    if (oid === f.oid && !finish) { started(); await new Promise(resolve => { finish = resolve; }); }
  });
  const reload = f.receive({ type: 'refresh' });
  await began;
  await f.receive({ type: 'copyCommitSummary', oid: f.oid });
  await f.receive({ type: 'selectCommit', oid: f.next });
  await f.receive({ type: 'copyCommitSummary', oid: f.next });
  finish();
  await reload;
  assert.deepEqual(f.copies, ['aaaaaaa first subject', 'bbbbbbb second subject']);
  assert.equal(f.provider.selected, f.next);
});

test('commit-file shortcut copies the current path and rejects stale file IDs', async t => {
  const f = await fixture(t);
  const file = f.messages.at(-1).files[0];
  await f.receive({ type: 'copyCommitFile', id: file.id });
  assert.deepEqual(f.copies, ['src/example.rs']);
  await f.receive({ type: 'selectCommit', oid: f.next });
  await f.receive({ type: 'copyCommitFile', id: file.id });
  await f.receive({ type: 'copyCommitFile', id: 'fabricated' });
  assert.deepEqual(f.copies, ['src/example.rs']);
  f.setFailure(true);
  await f.receive({ type: 'copyCommitFile', id: f.messages.at(-1).files[0].id });
  assert.match(f.errors.at(-1), /could not copy.*clipboard unavailable/i);
});

test('commit copy reports clipboard failures', async t => {
  const f = await fixture(t);
  f.setFailure(true);
  await f.receive({ type: 'copyCommitSummary', oid: f.oid });
  assert.match(f.errors.at(-1), /could not copy.*clipboard unavailable/i);
  assert.deepEqual(f.copies, []);
});

test('commit actions revalidate submodule ancestry after the picker', async t => {
  const f = await fixture(t);
  let ancestryCurrent = true;
  f.provider.validateGuard = async () => ancestryCurrent;
  f.setChoice(choices => { ancestryCurrent = false; return choices[0]; });
  await f.receive({ type: 'commitActions', oid: f.oid });
  assert.deepEqual(f.copies, []);
});

test('a closed panel or invalid commit fields cannot copy', async t => {
  const f = await fixture(t);
  const row = f.provider.rows.get(f.oid);
  f.provider.rows.set(f.oid, { ...row, short_id: 'different' });
  await f.receive({ type: 'commitActions', oid: f.oid });
  assert.match(f.errors.at(-1), /invalid commit details/i);
  f.provider.rows.set(f.oid, row);
  let finish, started;
  const began = new Promise(resolve => { started = resolve; });
  f.setChoice(choices => new Promise(resolve => { finish = () => resolve(choices[0]); started(); }));
  const pending = f.receive({ type: 'commitActions', oid: f.oid });
  await began;
  f.panel.dispose();
  finish();
  await pending;
  assert.deepEqual(f.copies, []);
});
