'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ComparisonGraph } = require('../src/comparison-graph');
class CancellationTokenSource { constructor() { this.token = {}; } cancel() {} dispose() {} }

test('graph comparison selects commits and reports ref movement without changing the diff', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), replacement = 'c'.repeat(40), identity = 'd'.repeat(64);
  const refs = { main: first, feature: second };
  const posted = [];
  const snapshot = { generation: 1, executable: '/gitpane', roots: ['/repo'], rootSources: ['/repo'], rootIdentities: [identity] };
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => snapshot, isCurrentMutation: () => true, createClient() { return {
      async initialize() {}, dispose() {}, async shutdown() {}, async request(method, input) {
        if (method === 'resolve_revision') return { oid: refs[input.revision] || input.revision, repo_identity: identity };
        return { base_oid: input.from, to_oid: input.to, repo_identity: identity, total: 1, next: null,
          files: [{ status: 'M', path: 'file.txt', is_submodule: false }] };
      }
    }; } };
  const repo = { rootUri: { fsPath: '/repo' }, state: { HEAD: { name: 'main' },
    workingTreeChanges: [{}], indexChanges: [], mergeChanges: [] } };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', interactive: true, panel: { visible: true }, graphGeneration: 1,
    rows: new Map([[first, { oid: first }], [second, { oid: second }]]),
    validateGraph: async () => true, post: state => posted.push(state) };
  let input = 'main';
  const vscode = { CancellationTokenSource, window: { showInputBox: async () => input } };
  const graph = new ComparisonGraph(provider, vscode, repo, 1, () => true);
  await graph.receive({ type: 'comparisonPick', which: 'from' });
  input = 'feature';
  await graph.receive({ type: 'comparisonPick', which: 'to' });
  assert.equal(posted.at(-1).dirty, true);
  assert.equal(posted.at(-1).files[0].path, 'file.txt');
  repo.state.workingTreeChanges = [];
  repo.state.untrackedChanges = [{}];
  graph.publish();
  assert.equal(posted.at(-1).dirty, true);
  repo.state.untrackedChanges = [];
  graph.publish();
  assert.equal(posted.at(-1).dirty, false);
  assert.equal(posted.at(-1).from.oid, first);
  refs.main = replacement;
  await graph.checkRefs();
  assert.equal(posted.at(-1).from.moved, true);
  assert.equal(posted.at(-1).from.oid, first);
  assert.equal(posted.at(-1).files[0].path, 'file.txt');
  await graph.receive({ type: 'comparisonRefresh' });
  assert.equal(posted.at(-1).from.oid, replacement);
  provider.rows.set(first, { oid: first, labels: [{ name: 'main', is_remote: false, is_tag: false, is_stash: false }] });
  refs['refs/heads/main'] = replacement;
  await graph.setRow('from', first, 0);
  assert.match(posted.at(-1).error, /moved/i);
  assert.equal(posted.at(-1).from, undefined);
  graph.model.clear();
  provider.selected = first;
  await graph.receive({ type: 'comparisonSelect', anchor: first, oid: second });
  assert.equal(posted.at(-1).from.oid, first);
  assert.equal(posted.at(-1).to.oid, second);
  graph.dispose();
});

test('a delayed older selection cannot replace a newer endpoint', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'd'.repeat(64);
  let release, entered;
  const waiting = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  let validations = 0;
  const posted = [];
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => ({ generation: 1, executable: '/gitpane', roots: ['/repo'] }), isCurrentMutation: () => true,
    createClient: () => ({ async initialize() {}, async request(_method, input) { return { oid: input.revision, repo_identity: identity }; },
      dispose() {}, async shutdown() {} }) };
  const repo = { rootUri: { fsPath: '/repo' } };
  const provider = { backend, tree: {}, links: {}, rows: new Map([[first, { oid: first }], [second, { oid: second }]]),
    selected: first, repoName: 'repo', post: state => posted.push(state),
    panel: { visible: true }, graphGeneration: 1, interactive: true,
    async validateGraph() { if (++validations === 1) { entered(); await held; } return true; } };
  const graph = new ComparisonGraph(provider, { window: {} }, repo, 1, () => true);
  const older = graph.receive({ type: 'comparisonSetSelected', which: 'from' });
  await waiting;
  provider.selected = second;
  await graph.receive({ type: 'comparisonSetSelected', which: 'from' });
  release();
  await older;
  assert.equal(posted.at(-1).from.oid, second);
  graph.dispose();
});

test('modifier click waits for the authenticated pending graph selection', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'd'.repeat(64);
  let release;
  const selection = new Promise(resolve => { release = resolve; });
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => ({ generation: 1, executable: '/gitpane', roots: ['/repo'] }), isCurrentMutation: () => true,
    createClient: () => ({ async initialize() {}, async request(method, input) {
      if (method === 'resolve_revision') return { oid: input.revision, repo_identity: identity };
      return { base_oid: input.from, to_oid: input.to, repo_identity: identity, total: 0, next: null, files: [] };
    }, dispose() {}, async shutdown() {} }) };
  const repo = { rootUri: { fsPath: '/repo' } };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', post() {}, panel: { visible: true },
    graphGeneration: 1, interactive: true, selectionEpoch: 4, rows: new Map([[first, { oid: first }], [second, { oid: second }]]),
    pendingSelection: { oid: first, selection: 4, promise: selection }, validateGraph: async () => true };
  const graph = new ComparisonGraph(provider, { window: {} }, repo, 1, () => true);
  const action = graph.receive({ type: 'comparisonSelect', anchor: first, oid: second });
  provider.selected = first;
  release();
  await action;
  assert.equal(graph.model.from.oid, first);
  assert.equal(graph.model.to.oid, second);
  graph.dispose();
  let releaseOld;
  const oldSelection = new Promise(resolve => { releaseOld = resolve; });
  provider.selected = undefined;
  provider.pendingSelection = { oid: first, selection: 4, promise: oldSelection };
  const newerGraph = new ComparisonGraph(provider, { window: {} }, repo, 1, () => true);
  const oldAction = newerGraph.receive({ type: 'comparisonSelect', anchor: first, oid: second });
  provider.selected = second;
  await newerGraph.receive({ type: 'comparisonSetSelected', which: 'from' });
  provider.selected = first;
  releaseOld();
  await oldAction;
  assert.equal(newerGraph.model.from.oid, second);
  assert.equal(newerGraph.model.to, undefined);
  newerGraph.dispose();
});

test('hiding the graph cancels an open revision picker', async () => {
  let release, cancelled = false;
  const input = new Promise(resolve => { release = resolve; });
  const backend = { externalSelections: new Map() };
  const repo = { rootUri: { fsPath: '/repo' } };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', post() {}, panel: { visible: true },
    graphGeneration: 1, interactive: true };
  const graph = new ComparisonGraph(provider, { CancellationTokenSource: class extends CancellationTokenSource { cancel() { cancelled = true; } },
    window: { showInputBox: () => input } }, repo, 1, () => true);
  const action = graph.receive({ type: 'comparisonPick', which: 'from' });
  provider.panel.visible = false;
  graph.suspend();
  provider.panel.visible = true;
  release('main');
  await action;
  assert.equal(cancelled, true);
  assert.equal(graph.model.from, undefined);
  graph.dispose();
});

test('hiding an unfinished comparison does not report an empty result', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'd'.repeat(64);
  let entered, release;
  const started = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const posted = [];
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => ({ generation: 1, executable: '/gitpane', roots: ['/repo'] }), isCurrentMutation: () => true,
    createClient: () => ({ async initialize() {}, async request(method, input) {
      if (method === 'resolve_revision') return { oid: input.revision, repo_identity: identity };
      entered(); await held;
      return { base_oid: first, to_oid: second, repo_identity: identity, total: 0, next: null, files: [] }; },
      dispose() {}, async shutdown() {} }) };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', post: state => posted.push(state),
    panel: { visible: true }, graphGeneration: 1, interactive: true, validateGraph: async () => true };
  let input = first;
  const graph = new ComparisonGraph(provider, { CancellationTokenSource, window: { showInputBox: async () => input } },
    { rootUri: { fsPath: '/repo' } }, 1, () => true);
  await graph.receive({ type: 'comparisonPick', which: 'from' });
  input = second;
  const action = graph.receive({ type: 'comparisonPick', which: 'to' });
  await started;
  provider.panel.visible = false;
  graph.suspend();
  release();
  await action;
  provider.panel.visible = true;
  graph.publish();
  assert.equal(posted.at(-1).hasResult, false);
  assert.equal(posted.at(-1).loading, false);
  assert.equal(posted.at(-1).from.oid, first);
  graph.dispose();
});

test('ref picker searches all pages and rejects a changed ref list', async () => {
  const oid = 'a'.repeat(40), identity = 'd'.repeat(64), revision = 'e'.repeat(64);
  let changed = false, moved = false, replaced = false, offered;
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => ({ generation: 1, executable: '/gitpane', roots: ['/repo'] }), isCurrentMutation: () => true,
    createClient: () => ({ async initialize() {}, async request(method, input) {
      if (method === 'resolve_revision') return { oid: moved ? 'b'.repeat(40) : oid, repo_identity: replaced ? 'c'.repeat(64) : identity };
      const count = input.offset === 0 ? 256 : 1;
      return { revision: changed && input.offset ? 'f'.repeat(64) : revision, repo_identity: identity,
        total: 257, next: input.offset === 0 ? 256 : null,
        refs: Array.from({ length: count }, (_, index) => ({ name: `refs/heads/topic-${input.offset + index}`, oid })) };
    }, dispose() {}, async shutdown() {} }) };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', post() {}, panel: { visible: true },
    graphGeneration: 1, interactive: true, validateGraph: async () => true };
  const vscode = { CancellationTokenSource: class { constructor() { this.token = {}; } cancel() {} dispose() {} },
    window: { async showQuickPick(choices) { offered = choices; return choices.at(-1); } } };
  const graph = new ComparisonGraph(provider, vscode, { rootUri: { fsPath: '/repo' } }, 1, () => true);
  await graph.receive({ type: 'comparisonBrowse', which: 'from' });
  assert.equal(offered.length, 257);
  assert.equal(graph.model.from.revision, 'refs/heads/topic-256');
  changed = true;
  await graph.receive({ type: 'comparisonBrowse', which: 'to' });
  assert.match(graph.error, /changed ref list/i);
  assert.equal(graph.model.to, undefined);
  changed = false;
  moved = true;
  await graph.receive({ type: 'comparisonBrowse', which: 'to' });
  assert.match(graph.error, /ref or repository changed/i);
  assert.equal(graph.model.to, undefined);
  graph.model.clear();
  moved = false;
  replaced = true;
  await graph.receive({ type: 'comparisonBrowse', which: 'from' });
  assert.match(graph.error, /ref or repository changed/i);
  assert.equal(graph.model.from, undefined);
  graph.dispose();
});

test('hiding a graph dismisses an open ref picker', async () => {
  const identity = 'd'.repeat(64), oid = 'a'.repeat(40);
  let opened, close, cancelled = false;
  const started = new Promise(resolve => { opened = resolve; });
  const picker = new Promise(resolve => { close = resolve; });
  const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
    mutationSnapshot: async () => ({ generation: 1, executable: '/gitpane', roots: ['/repo'] }), isCurrentMutation: () => true,
    createClient: () => ({ async initialize() {}, async request() { return { revision: 'e'.repeat(64), repo_identity: identity,
      total: 1, next: null, refs: [{ name: 'refs/heads/main', oid }] }; }, dispose() {}, async shutdown() {} }) };
  const provider = { backend, tree: {}, links: {}, repoName: 'repo', post() {}, panel: { visible: true },
    graphGeneration: 1, interactive: true, validateGraph: async () => true };
  const vscode = { CancellationTokenSource: class { constructor() { this.token = {}; } cancel() { cancelled = true; close(); } dispose() {} },
    window: { showQuickPick() { opened(); return picker; } } };
  const graph = new ComparisonGraph(provider, vscode, { rootUri: { fsPath: '/repo' } }, 1, () => true);
  const action = graph.receive({ type: 'comparisonBrowse', which: 'from' });
  await started;
  provider.panel.visible = false;
  graph.suspend();
  await action;
  assert.equal(cancelled, true);
  assert.equal(graph.model.from, undefined);
  graph.dispose();
});
