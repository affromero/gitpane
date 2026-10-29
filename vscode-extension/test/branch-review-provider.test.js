'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { BranchReviewProvider } = require('../src/branch-review-provider');

function fixture() {
  const root = path.resolve('workspace/repo');
  const base = 'a'.repeat(40);
  const head = 'b'.repeat(40);
  const repo = { rootUri: { fsPath: root, toString: () => root }, state: { HEAD: { commit: head } } };
  let visible = true;
  let review = { base_ref: 'origin/main', base_oid: base, head_oid: head, files: [] };
  let choose = choices => choices[0];
  const calls = [];
  const errors = [];
  const information = [];
  const tree = { git: { repositories: [repo] }, isVisible: () => visible };
  const vscode = {
    window: {
      showQuickPick: async choices => choose(choices),
      showErrorMessage: async message => errors.push(message),
      showInformationMessage: async message => information.push(message)
    },
    Uri: { file: value => ({ fsPath: value, with: changes => ({ fsPath: value, ...changes }) }) },
    commands: { executeCommand: async (...args) => calls.push(args) }
  };
  const git = { toGitUri: (uri, ref) => ({ fsPath: uri.fsPath, ref }) };
  const provider = new BranchReviewProvider(tree, { branchReview: async () => {
    if (review instanceof Error) throw review;
    return review;
  } }, git, { vscode });
  return {
    provider, repo, root, base, head, calls, errors, information,
    setReview(value) { review = value; }, getReview() { return review; },
    setVisible(value) { visible = value; }, pick(fn) { choose = fn; }
  };
}

test('opens committed additions, modifications and deletions against the merge base', async () => {
  const f = fixture();
  const files = [
    { path: 'new.txt', status: 'A', is_submodule: false },
    { path: 'changed.txt', status: 'M', is_submodule: false },
    { path: 'deleted.txt', status: 'D', is_submodule: false },
    { path: 'new/name.txt', old_path: 'old/name.txt', status: 'R', is_submodule: false }
  ];
  f.setReview({ ...f.getReview(), files });
  for (const file of files) {
    f.pick(choices => choices.find(choice => choice.file.path === file.path));
    await f.provider.open(f.repo);
  }
  assert.equal(f.calls.length, 4);
  assert.equal(f.calls[0][0], 'vscode.diff');
  assert.equal(f.calls[0][1].scheme, 'gitpane-empty');
  assert.equal(f.calls[0][2].ref, f.head);
  assert.equal(f.calls[1][1].ref, f.base);
  assert.equal(f.calls[1][2].ref, f.head);
  assert.equal(f.calls[2][1].ref, f.base);
  assert.equal(f.calls[2][2].scheme, 'gitpane-empty');
  assert.equal(f.calls[3][1].fsPath, path.join(f.root, 'old/name.txt'));
  assert.equal(f.calls[3][1].ref, f.base);
  assert.equal(f.calls[3][2].fsPath, path.join(f.root, 'new/name.txt'));
  assert.equal(f.calls[3][2].ref, f.head);
});

test('reports empty reviews and backend failures', async () => {
  const f = fixture();
  await f.provider.open(f.repo);
  assert.match(f.information[0], /no committed changes.*origin\/main/i);
  f.setReview(new Error('No review base resolved'));
  await f.provider.open(f.repo);
  assert.match(f.errors[0], /no review base resolved/i);
  assert.equal(f.calls.length, 0);
});

test('does not open a selected file after the repository or HEAD changes', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: 'file', status: 'M', is_submodule: false }] });
  f.pick(choices => { f.setVisible(false); return choices[0]; });
  await f.provider.open(f.repo);
  assert.equal(f.calls.length, 0);
  assert.match(f.errors[0], /repository or head changed/i);
  f.setVisible(true);
  f.pick(choices => { f.repo.state.HEAD.commit = 'c'.repeat(40); return choices[0]; });
  await f.provider.open(f.repo);
  assert.equal(f.calls.length, 0);
  assert.match(f.errors[1], /repository or head changed/i);
});

test('rejects escaped paths and submodules instead of opening unsafe diffs', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: '../outside', status: 'M', is_submodule: false }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[0], /invalid branch review file/i);
  f.setReview({ ...f.getReview(), files: [{ path: 'new', old_path: '../outside', status: 'R', is_submodule: false }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[1], /invalid branch review file/i);
  f.setReview({ ...f.getReview(), files: [{ path: 'module', status: 'M', is_submodule: true }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[2], /submodule changes cannot be opened/i);
  assert.equal(f.calls.length, 0);
});
