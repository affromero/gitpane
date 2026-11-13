'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

module.exports = async function testExternalWorktree({ vscode, provider, snapshots, documents, diffs, errors, receive, gitAt, repoPath, siblingPath }) {
  const tracked = path.join(siblingPath, 'external-tracked.txt');
  const untracked = path.join(siblingPath, 'external-untracked.txt');
  const pointerOld = gitAt(siblingPath, 'rev-parse', 'HEAD');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-child.path', 'external-child');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-child.url', '../external-child');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-child.ignore', 'all');
  fs.writeFileSync(path.join(siblingPath, 'external-mixed'), 'original file\n');
  gitAt(siblingPath, 'add', '.gitmodules', 'external-mixed');
  gitAt(siblingPath, 'update-index', '--add', '--cacheinfo', '160000', pointerOld, 'external-child');
  gitAt(siblingPath, 'commit', '-qm', 'external pointer base');
  const pointerNew = gitAt(siblingPath, 'rev-parse', 'HEAD');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-mixed.path', 'external-mixed');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-mixed.url', '../external-mixed');
  gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-mixed.ignore', 'all');
  gitAt(siblingPath, 'add', '.gitmodules');
  gitAt(siblingPath, 'update-index', '--cacheinfo', '160000', pointerNew, 'external-child');
  gitAt(siblingPath, 'update-index', '--force-remove', 'external-mixed');
  fs.unlinkSync(path.join(siblingPath, 'external-mixed'));
  gitAt(siblingPath, 'update-index', '--add', '--cacheinfo', '160000', pointerNew, 'external-mixed');
  fs.writeFileSync(tracked, 'committed\n');
  gitAt(siblingPath, 'add', '--', 'external-tracked.txt');
  gitAt(siblingPath, 'commit', '-qm', 'external fixture');
  const originalBlob = gitAt(siblingPath, 'rev-parse', `${pointerNew}:external-mixed`);
  const replacement = path.join(path.dirname(siblingPath), 'replacement-blob');
  fs.writeFileSync(replacement, 'replacement file\n');
  const replacementBlob = gitAt(siblingPath, 'hash-object', '-w', replacement);
  gitAt(siblingPath, 'replace', originalBlob, replacementBlob);
  assert.equal(gitAt(siblingPath, 'cat-file', '-p', originalBlob), 'replacement file');
  gitAt(siblingPath, '-c', 'protocol.file.allow=always', 'clone', '-q', '--no-checkout', siblingPath, 'external-child');
  gitAt(path.join(siblingPath, 'external-child'), 'checkout', '-q', '--detach', pointerNew);
  gitAt(siblingPath, '-c', 'protocol.file.allow=always', 'clone', '-q', '--no-checkout', siblingPath, 'external-mixed');
  gitAt(path.join(siblingPath, 'external-mixed'), 'checkout', '-q', '--detach', pointerNew);
  fs.writeFileSync(tracked, 'staged\n');
  gitAt(siblingPath, 'add', '--', 'external-tracked.txt');
  fs.writeFileSync(tracked, 'working\n');
  fs.writeFileSync(untracked, 'new file\n');

  const folders = vscode.workspace.workspaceFolders.map(folder => folder.uri.fsPath);
  const previousErrors = errors.length;
  await provider.tree.refresh();
  const row = () => snapshots.at(-1).repositories.find(item => item.name === 'repo').worktrees
    .find(item => item.name === 'outside sibling');
  const selected = row();
  assert.ok(selected?.external, 'the sibling remains outside the workspace');
  assert.equal(selected.available, false, 'status is visible before inspection');
  assert.ok(!selected.statusUnavailable, `outside worktree status unavailable: ${selected.statusError}; Git: ${gitAt(siblingPath, 'status', '--porcelain=v1')}`);
  assert.equal(selected.dirty, true, `outside worktree status: ${JSON.stringify(selected)}; Git: ${gitAt(siblingPath, 'status', '--porcelain=v1')}`);
  assert.equal(selected.fileCount, 2);
  const comparison = await require('./comparison-external-real')({ vscode, provider, documents, diffs, errors, gitAt, siblingPath, selected, pointerOld, pointerNew });
  await receive({ type: 'loadChanges', id: selected.id });
  assert.equal(errors.length, previousErrors, errors.at(-1));
  {
    gitAt(siblingPath, 'config', '-f', '.gitmodules', '--unset', 'submodule.external-child.ignore');
    const child = path.join(siblingPath, 'external-child');
    fs.writeFileSync(path.join(child, 'worktree-only.txt'), 'worktree submodule change\n');
    gitAt(child, 'add', 'worktree-only.txt');
    gitAt(child, 'commit', '-qm', 'worktree submodule update');
    await provider.tree.refresh();
    assert.ok(row()?.submodules?.some(item => item.name === 'external-child'),
      'an initialized submodule must appear under its worktree');
    fs.writeFileSync(path.join(child, 'uncommitted.txt'), 'child work in progress\n');
    await provider.tree.refresh();
    const childRow = row().submodules.find(item => item.name === 'external-child');
    assert.equal(childRow.dirty, true, 'worktree child status reflects its own changes');
    await receive({ type: 'selectOwner', id: childRow.id });
    assert.equal(provider.workspaceViews.selectedId, childRow.id);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(fs.realpathSync.native(provider.workspaceViews.graph.currentRepo()?.rootUri.fsPath), fs.realpathSync.native(child),
      'the graph follows the worktree submodule');
    await receive({ type: 'loadChanges', id: childRow.id });
    const changedChild = row().submodules.find(item => item.name === 'external-child');
    const childFile = changedChild.groups.flatMap(group => group.files).find(file => file.path === 'uncommitted.txt');
    assert.ok(childFile, 'the selected child shows its own changed files');
    const childDiffs = diffs.length;
    await receive({ type: 'openChange', id: childFile.id });
    if (process.platform === 'win32') {
      assert.equal(diffs.length, childDiffs);
      assert.match(errors.at(-1), /file reads are unavailable on this platform/i);
      errors.splice(previousErrors);
    } else {
      assert.equal(diffs.length, childDiffs + 1, errors.at(-1));
      assert.equal((await documents.get('gitpane-blob').readFile(diffs.at(-1)[1])).toString(), 'child work in progress\n');
    }
    const childSelection = provider.externalOwners.get(childRow.id).selection;
    gitAt(siblingPath, 'config', '-f', '.gitmodules', '--unset', 'submodule.external-child.path');
    await assert.rejects(provider.tree.backend.client.request('external_worktree_read', {
      selection: childSelection, method: 'status', params: { repo: child, include_worktrees: false, summary_only: true }
    }), /registered|submodule|changed/i);
    gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-child.path', 'external-child');
    gitAt(siblingPath, 'config', '-f', '.gitmodules', 'submodule.external-child.ignore', 'all');
    gitAt(child, 'reset', '--hard', '-q', pointerNew);
    fs.unlinkSync(path.join(child, 'uncommitted.txt'));
    await provider.tree.refresh();
  }
  assert.deepEqual(vscode.workspace.workspaceFolders.map(folder => folder.uri.fsPath), folders);
  assert.ok(!provider.tree.backend.roots.some(root => path.resolve(root) === path.resolve(siblingPath)));

  const file = (group, name) => row()?.groups.find(item => item.id === group)?.files.find(item => item.path === name);
  const staged = file('index', 'external-tracked.txt');
  const working = file('working', 'external-tracked.txt');
  const added = file('untracked', 'external-untracked.txt');
  assert.ok(staged && working && added, 'all three changes must be selectable');
  const blobs = documents.get('gitpane-blob');
  assert.equal((await blobs.readFile(comparison.file)).toString(), 'committed\n');
  for (const pointer of comparison.pointers) assert.match(await documents.get('gitpane-link').provideTextDocumentContent(pointer), /^Subproject commit /);
  let count = diffs.length;
  await receive({ type: 'openChange', id: staged.id });
  assert.equal(diffs.length, ++count, errors.at(-1));
  const [head, index] = diffs.at(-1);
  assert.equal((await blobs.readFile(head)).toString(), 'committed\n');
  assert.equal((await blobs.readFile(index)).toString(), 'staged\n');

  await receive({ type: 'fileMenuRequest', id: staged.id, requestId: 1 });
  const menu = snapshots.pop();
  assert.equal(menu.type, 'fileMenu');
  assert.deepEqual(menu.choices.map(choice => choice.action), ['open', 'copy']);
  await receive({ type: 'fileMenuAction', id: staged.id, token: menu.token, action: 'open' });
  assert.equal(diffs.length, ++count, 'outside worktree menu opens its selected diff');

  await receive({ type: 'openChange', id: working.id });
  if (process.platform === 'win32') {
    assert.equal(diffs.length, count);
    assert.match(errors.at(-1), /file reads are unavailable on this platform/i);
    await receive({ type: 'openChange', id: added.id });
    assert.equal(diffs.length, count);
    assert.match(errors.at(-1), /file reads are unavailable on this platform/i);
    assert.equal(errors.length, previousErrors + 2);
    errors.splice(previousErrors);
  } else {
    assert.equal(diffs.length, ++count, errors.at(-1));
    const [indexSide, workingSide] = diffs.at(-1);
    assert.equal((await blobs.readFile(indexSide)).toString(), 'staged\n');
    assert.equal(workingSide.scheme, 'gitpane-blob');
    assert.equal((await blobs.readFile(workingSide)).toString(), 'working\n');

    await receive({ type: 'openChange', id: added.id });
    assert.equal(diffs.length, ++count, errors.at(-1));
    assert.equal(diffs.at(-1)[0].scheme, 'gitpane-empty');
    const untrackedSide = diffs.at(-1)[1];
    assert.equal(untrackedSide.scheme, 'gitpane-blob');
    assert.equal((await blobs.readFile(untrackedSide)).toString(), 'new file\n');

    const secret = path.join(path.dirname(siblingPath), 'external-secret.txt');
    fs.writeFileSync(secret, 'outside content\n');
    fs.unlinkSync(tracked);
    fs.symlinkSync(secret, tracked);
    await assert.rejects(blobs.readFile(workingSide), /no longer|changed|stale|unavailable|symbolic|symlink/i,
      'a replaced file cannot read through a symlink outside the worktree');
    fs.unlinkSync(tracked);
    fs.writeFileSync(tracked, 'working\n');
    fs.unlinkSync(secret);
    vscode.workspace.isTrusted = false;
    await assert.rejects(blobs.readFile(indexSide), /no longer|changed|stale|unavailable/i,
      'an open diff cannot read Git content after workspace trust is lost');
    await assert.rejects(blobs.readFile(workingSide), /no longer|changed|stale|unavailable/i,
      'an open working file cannot be read after workspace trust is lost');
    vscode.workspace.isTrusted = true;
    const deadline = Date.now() + 5000;
    while (provider.links.refreshing) {
      assert.ok(Date.now() < deadline, 'the existing diff refresh must finish');
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    let started, release;
    const entered = new Promise(resolve => { started = resolve; });
    const gate = new Promise(resolve => { release = resolve; });
    const originalOperation = provider.tree.backend.gitSideOperation;
    provider.tree.backend.gitSideOperation = async function(session, ...args) {
      if (session.external && args[1].kind === 'metadata') {
        started();
        await gate;
        throw new Error('Git configuration lookup failed after worktree removal');
      }
      return originalOperation.call(this, session, ...args);
    };
    const beforeRefreshErrors = errors.length;
    const refresh = provider.links.refresh();
    let timer;
    try {
      await Promise.race([entered, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('The diff metadata check did not start')), 5000);
      })]);
    } finally { clearTimeout(timer); }
    gitAt(repoPath, 'worktree', 'remove', '--force', siblingPath);
    release();
    await refresh;
    provider.tree.backend.gitSideOperation = originalOperation;
    assert.equal(errors.length, beforeRefreshErrors, 'a stale worktree must not report a refresh failure');
    await assert.rejects(blobs.readFile(head), /no longer|changed|stale|unavailable/i,
      'an open diff cannot retain access after worktree removal');
    await assert.rejects(blobs.readFile(comparison.file), /no longer|changed|stale|unavailable/i,
      'a comparison closed with its graph cannot retain access after worktree removal');
    await assert.rejects(blobs.readFile(comparison.mixedFile), /no longer|changed|stale|unavailable/i,
      'a mixed comparison cannot retain file access after worktree removal');
    for (const pointer of comparison.pointers) await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(pointer),
      /no longer|changed|stale|unavailable/i, 'a pointer diff cannot retain access after worktree removal');
    gitAt(repoPath, 'worktree', 'add', '-qb', 'feature/replacement', siblingPath);
    fs.writeFileSync(path.join(siblingPath, 'external-tracked.txt'), 'replacement\n');
    await assert.rejects(blobs.readFile(workingSide), /no longer|changed|stale|unavailable/i,
      'an open working file cannot read from a replacement worktree');
    await assert.rejects(blobs.readFile(untrackedSide), /no longer|changed|stale|unavailable/i,
      'an open untracked file cannot read from a replacement worktree');
    gitAt(repoPath, 'worktree', 'remove', '--force', siblingPath);
  }
  if (process.platform === 'win32') {
    vscode.workspace.isTrusted = false;
    await assert.rejects(blobs.readFile(head), /no longer|changed|stale|unavailable/i,
      'an open diff cannot read Git content after workspace trust is lost');
    vscode.workspace.isTrusted = true;
    gitAt(repoPath, 'worktree', 'remove', '--force', siblingPath);
    await assert.rejects(blobs.readFile(head), /no longer|changed|stale|unavailable/i,
      'an open diff cannot retain access after worktree removal');
    await assert.rejects(blobs.readFile(comparison.file), /no longer|changed|stale|unavailable/i,
      'a comparison closed with its graph cannot retain access after worktree removal');
    for (const pointer of comparison.pointers) await assert.rejects(documents.get('gitpane-link').provideTextDocumentContent(pointer),
      /no longer|changed|stale|unavailable/i, 'a pointer diff cannot retain access after worktree removal');
  }
  gitAt(repoPath, 'branch', '-D', 'feature/sibling', ...(process.platform === 'win32' ? [] : ['feature/replacement']));
  assert.deepEqual(vscode.workspace.workspaceFolders.map(folder => folder.uri.fsPath), folders);
  console.log(process.platform === 'win32'
    ? 'PASS external staged diffs and explicit unsupported working-file errors without changing the workspace'
    : 'PASS external staged, unstaged and untracked native diffs without changing the workspace');
};
