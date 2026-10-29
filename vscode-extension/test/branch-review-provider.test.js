'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { BranchReviewProvider } = require('../src/branch-review-provider');
const { installGitSideFixture } = require('./git-side-fixture');

function fixture() {
  const root = path.resolve('workspace/repo');
  const base = 'a'.repeat(40);
  const head = 'b'.repeat(40);
  const repo = { rootUri: { fsPath: root, toString: () => root }, state: { HEAD: { commit: head } },
    async buffer(ref) { return Buffer.from(ref); } };
  let visible = true;
  let incarnation = 1;
  let review = { base_ref: 'origin/main', base_oid: base, head_oid: head, repo_identity: 'f'.repeat(64), files: [] };
  let identityValid = true;
  let reviewHook = async () => {};
  let choose = choices => choices[0];
  const calls = [];
  const errors = [];
  const information = [];
  const tree = { git: { get repositories() { return [{ ...repo }]; } }, isVisible: () => visible,
    repositoryToken: () => incarnation, isCurrentRepository: (_, token) => visible && token === incarnation };
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
  const links = {
    async createLiveBinary(uri, metadata, read, guard) {
      assert.equal(await guard(), true);
      const details = await metadata();
      return { scheme: 'gitpane-blob', fsPath: uri.fsPath, ref: details.ref, guard, read: () => read(details) };
    },
    async validateStaticIdentity(_repo, identity, guard) { return identityValid && identity === 'f'.repeat(64) && guard(); },
    async createStatic(_uri, content, guard) {
      if (!await guard()) throw new Error('Repository identity changed');
      return { scheme: 'gitpane-link', content, guard };
    },
    async createBinary(_uri, metadata, read, guard) {
      if (!await guard()) throw new Error('Repository identity changed');
      assert.equal((await metadata()).size, 40);
      return { scheme: 'gitpane-blob', ref: (await read()).toString(), guard };
    }
  };
  const backend = { branchReview: async () => {
    if (review instanceof Error) throw review;
    const result = structuredClone(review);
    await reviewHook();
    return result;
  }, async repoBlobMetadata(repoPath, identity, ref) {
    assert.equal(repoPath, root);
    assert.equal(identity, 'f'.repeat(64));
    return { oid: '1'.repeat(40), mode: 0o100644, size: 40, head_oid: ref };
  }, async submoduleHistoricalContent(repoPath, identity, commit, relative, oid, mode) {
    assert.equal(repoPath, root);
    assert.equal(identity, 'f'.repeat(64));
    assert.equal(relative, 'module');
    assert.equal(oid, '3'.repeat(40));
    assert.equal(mode, 33188);
    assert.ok(commit === base || commit === head);
    return { size: 40 };
  } };
  installGitSideFixture(backend, () => repo);
  const provider = new BranchReviewProvider(tree, backend, git, { vscode, links });
  return {
    provider, repo, root, base, head, calls, errors, information,
    setReview(value) { review = value; }, getReview() { return review; },
    setVisible(value) { visible = value; }, setIdentityValid(value) { identityValid = value; },
    onReview(value) { reviewHook = value; },
    reopen() { incarnation++; }, pick(fn) { choose = fn; }
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

test('opens Unix filenames containing backslashes as literal names', { skip: process.platform === 'win32' }, async () => {
  const f = fixture();
  const oldPath = 'old\\..\\name.txt', newPath = 'new\\..\\name.txt';
  f.setReview({ ...f.getReview(), files: [{ path: newPath, old_path: oldPath, status: 'R', is_submodule: false }] });
  await f.provider.open(f.repo);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0][0], 'vscode.diff');
  assert.equal(f.calls[0][1].fsPath, path.join(f.root, oldPath));
  assert.equal(f.calls[0][1].ref, f.base);
  assert.equal(f.calls[0][2].fsPath, path.join(f.root, newPath));
  assert.equal(f.calls[0][2].ref, f.head);
  assert.deepEqual(f.errors, []);
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

test('does not open a review diff when its worktree link changes during selection', async () => {
  const f = fixture();
  let linked = true;
  f.setReview({ ...f.getReview(), files: [{ path: 'file', status: 'M', is_submodule: false }] });
  f.pick(choices => { linked = false; return choices[0]; });
  await f.provider.open(f.repo, () => linked);
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /repository or head changed/i);
});

test('does not open a review diff after the same repository path is reopened', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: 'file', status: 'M', is_submodule: false }] });
  f.pick(choices => { f.reopen(); return choices[0]; });
  await f.provider.open(f.repo);
  assert.deepEqual(f.calls, []);
  assert.match(f.errors[0], /repository or head changed/i);
});

test('rejects escaped paths and unverified submodule details', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: '../outside', status: 'M', is_submodule: false }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[0], /invalid branch review file/i);
  f.setReview({ ...f.getReview(), files: [{ path: 'new', old_path: '../outside', status: 'R', is_submodule: false }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[1], /invalid branch review file/i);
  f.setReview({ ...f.getReview(), files: [{ path: 'module', status: 'M', is_submodule: true }] });
  await f.provider.open(f.repo);
  assert.match(f.errors[2], /verified submodule pointer details/i);
  assert.equal(f.calls.length, 0);
});

test('opens branch review gitlinks using the compared tree endpoints', async () => {
  const f = fixture();
  const pointer = digit => ({ kind: 'gitlink', oid: digit.repeat(40), mode: 57344 });
  const blob = { kind: 'file', oid: '3'.repeat(40), mode: 33188 };
  const cases = [
    { status: 'A', old: null, new: pointer('1'), left: 'empty', right: '1'.repeat(40) },
    { status: 'M', old: pointer('1'), new: pointer('2'), left: '1'.repeat(40), right: '2'.repeat(40) },
    { status: 'D', old: pointer('1'), new: null, left: '1'.repeat(40), right: 'empty' },
    { status: 'M', old: blob, new: pointer('2'), left: f.base, right: '2'.repeat(40) },
    { status: 'M', old: pointer('1'), new: blob, left: '1'.repeat(40), right: f.head }
  ];
  const side = value => value.content ? value.content.match(/[a-f0-9]{40}/)[0] : value.ref ||
    (value.scheme === 'gitpane-empty' ? 'empty' : undefined);
  for (const item of cases) {
    f.setReview({ ...f.getReview(), files: [{ path: 'module', status: item.status, is_submodule: true,
      gitlink: { old: item.old, new: item.new } }] });
    await f.provider.open(f.repo);
    const diff = f.calls.at(-1);
    assert.equal(diff[0], 'vscode.diff');
    assert.equal(side(diff[1]), item.left);
    assert.equal(side(diff[2]), item.right);
  }
  assert.equal(f.calls.length, cases.length);
  assert.deepEqual(f.errors, []);
  const document = f.calls.at(-1)[1];
  assert.equal(await document.guard(), true);
  f.setIdentityValid(false);
  assert.equal(await document.guard(), false, 'a replaced repository invalidates its pointer document');
  f.setIdentityValid(true);
  f.reopen();
  assert.equal(await document.guard(), false, 'an open pointer document cannot follow a reopened repository');
});

test('does not open a gitlink diff if the repository reopens during verification', async () => {
  const f = fixture();
  const pointer = digit => ({ kind: 'gitlink', oid: digit.repeat(40), mode: 57344 });
  f.setReview({ ...f.getReview(), files: [{ path: 'module', status: 'M', is_submodule: true,
    gitlink: { old: pointer('1'), new: pointer('2') } }] });
  let calls = 0, release;
  f.onReview(async () => { if (++calls === 2) await new Promise(resolve => { release = resolve; }); });
  const pending = f.provider.open(f.repo);
  while (!release) await new Promise(resolve => setImmediate(resolve));
  f.reopen();
  release();
  await pending;
  assert.equal(f.calls.length, 0);
  assert.match(f.errors[0], /no longer/i);
});

test('an open pointer diff survives later sidebar selection changes', async () => {
  const f = fixture();
  const pointer = digit => ({ kind: 'gitlink', oid: digit.repeat(40), mode: 57344 });
  f.setReview({ ...f.getReview(), files: [{ path: 'module', status: 'M', is_submodule: true,
    gitlink: { old: pointer('1'), new: pointer('2') } }] });
  let selectionCurrent = true;
  await f.provider.open(f.repo, () => selectionCurrent);
  assert.equal(f.calls.length, 1);
  const document = f.calls[0][1];
  selectionCurrent = false;
  assert.equal(await document.guard(), true);
  f.reopen();
  assert.equal(await document.guard(), false);
});

test('open branch review files retain their worktree identity after sidebar selection changes', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: 'file', status: 'M', is_submodule: false }] });
  let selected = true, linked = true;
  const worktree = { current: () => linked, contextKey: 'parent-worktree' };
  await f.provider.open(f.repo, () => selected, worktree);
  assert.equal(f.calls.length, 1);
  const document = f.calls[0][1];
  assert.equal(await document.guard(), true);
  selected = false;
  assert.equal(await document.guard(), true);
  linked = false;
  assert.equal(await document.guard(), false);
});

test('rejects gitlink comparisons against a directory', async () => {
  const f = fixture();
  f.setReview({ ...f.getReview(), files: [{ path: 'module', status: 'M', is_submodule: true,
    gitlink: { old: { kind: 'gitlink', oid: '1'.repeat(40), mode: 57344 },
      new: { kind: 'tree', oid: '2'.repeat(40), mode: 16384 } } }] });
  await f.provider.open(f.repo);
  assert.equal(f.calls.length, 0);
  assert.match(f.errors[0], /directory/i);
});

test('rejects changed branch-review pointers, base or repository identity before opening', async () => {
  const pointer = digit => ({ kind: 'gitlink', oid: digit.repeat(40), mode: 57344 });
  for (const change of ['base', 'pointer', 'identity', 'reopen', 'link']) {
    const f = fixture();
    const file = { path: 'module', status: 'M', is_submodule: true,
      gitlink: { old: pointer('1'), new: pointer('2') } };
    f.setReview({ ...f.getReview(), files: [file] });
    let linked = true;
    f.pick(choices => {
      const review = structuredClone(f.getReview());
      if (change === 'base') review.base_oid = 'c'.repeat(40);
      if (change === 'pointer') review.files[0].gitlink.new = pointer('4');
      if (change === 'identity') review.repo_identity = 'e'.repeat(64);
      if (change === 'reopen') f.reopen();
      if (change === 'link') linked = false;
      f.setReview(review);
      return choices[0];
    });
    await f.provider.open(f.repo, () => linked);
    assert.equal(f.calls.length, 0, `${change} must not open a diff`);
    assert.match(f.errors[0], /changed|no longer/i);
  }
});
