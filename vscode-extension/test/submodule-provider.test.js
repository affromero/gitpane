'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { SubmoduleProvider } = require('../src/submodule-provider');

function fixture(t) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-submodule-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const parentPath = path.join(temporary, 'parent');
  const childPath = path.join(parentPath, 'child');
  fs.mkdirSync(childPath, { recursive: true });
  const uri = value => ({ fsPath: value, toString: () => value, with: changes => ({ fsPath: value, ...changes }) });
  const base = 'a'.repeat(40), head = 'b'.repeat(40);
  const parent = { rootUri: uri(parentPath), state: { submodules: [{ path: 'child' }] } };
  const node = { repo: parent, group: 'working', change: { uri: uri(childPath), status: 5 } };
  const child = { rootUri: uri(childPath), state: { HEAD: { commit: head }, submodules: [],
    workingTreeChanges: [{ uri: uri(path.join(childPath, 'dirty.txt')), status: 5 }] }, async status() {} };
  let visible = true;
  let pick = choices => choices[0];
  let response = { repo: childPath, base_oid: base, head_oid: head, pointer_files: [
    { status: 'R', path: 'new.txt', old_path: 'old.txt', is_submodule: false }
  ] };
  let opens = 0;
  const commands = [], errors = [], graphs = [];
  const tree = { isVisible: () => visible, isPathInWorkspace: value => value.startsWith(parentPath + path.sep) };
  const vscode = { Uri: { file: uri }, window: {
    showQuickPick: async choices => pick(choices), showErrorMessage: async text => errors.push(text)
  }, commands: { executeCommand: async (...args) => commands.push(args) } };
  const git = { openRepository: async () => { opens++; return child; }, toGitUri: (value, ref) => ({ fsPath: value.fsPath, ref }) };
  const provider = new SubmoduleProvider(tree, { inspectSubmodule: async () => {
    if (response instanceof Error) throw response;
    return response;
  } }, git, { vscode, resolveChange: () => visible ? node : undefined, openGraph: async repo => graphs.push(repo) });
  return { temporary, childPath, node, child, base, head, provider, commands, errors, graphs,
    opened: () => opens, pick(fn) { pick = fn; }, setVisible(value) { visible = value; },
    setResponse(value) { response = value; }, getResponse() { return response; } };
}

test('opens committed rename and current dirty file comparisons using native editors', async t => {
  const f = fixture(t);
  f.pick(choices => choices.find(choice => choice.kind === 'pointer'));
  await f.provider.open(f.node);
  assert.equal(f.commands[0][0], 'vscode.diff');
  assert.equal(f.commands[0][1].fsPath, path.join(f.childPath, 'old.txt'));
  assert.equal(f.commands[0][1].ref, f.base);
  assert.equal(f.commands[0][2].fsPath, path.join(f.childPath, 'new.txt'));
  assert.equal(f.commands[0][2].ref, f.head);
  f.pick(choices => choices.find(choice => choice.kind === 'change'));
  await f.provider.open(f.node);
  assert.equal(f.commands[1][1].ref, '');
  assert.equal(f.commands[1][2].fsPath, path.join(f.childPath, 'dirty.txt'));
  f.pick(choices => choices.find(choice => choice.kind === 'graph'));
  await f.provider.open(f.node);
  assert.deepEqual(f.graphs, [f.child]);
});

test('reports uninitialized submodules and refuses outside paths before opening a repository', async t => {
  const f = fixture(t);
  f.setResponse(new Error('Submodule is not initialized'));
  await f.provider.open(f.node);
  assert.match(f.errors[0], /not initialized/i);
  f.setResponse({ repo: path.join(f.temporary, 'outside'), base_oid: null, head_oid: f.head, pointer_files: [] });
  await f.provider.open(f.node);
  assert.match(f.errors[1], /no longer in the workspace/i);
  assert.equal(f.opened(), 0);
});

test('rejects a removed repository or changed HEAD after the picker', async t => {
  const f = fixture(t);
  f.pick(choices => { f.setVisible(false); return choices[0]; });
  await f.provider.open(f.node);
  assert.match(f.errors[0], /changed/i);
  assert.equal(f.commands.length, 0);
  f.setVisible(true);
  f.pick(choices => { f.child.state.HEAD.commit = 'c'.repeat(40); return choices[0]; });
  await f.provider.open(f.node);
  assert.match(f.errors[1], /changed/i);
  assert.equal(f.graphs.length, 0);
});

test('rejects invalid rename paths before opening any diff', async t => {
  const f = fixture(t);
  f.setResponse({ ...f.getResponse(), pointer_files: [{ status: 'R', path: 'new', old_path: '../outside', is_submodule: false }] });
  await f.provider.open(f.node);
  assert.match(f.errors[0], /invalid submodule change/i);
  assert.equal(f.commands.length, 0);
});
