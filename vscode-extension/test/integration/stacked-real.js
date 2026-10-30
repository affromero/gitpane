'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const Module = require('node:module');
const path = require('node:path');

module.exports = async ({ views, receive, snapshots, diffs, errors, repoPath, worktreePath }) => {
  const lower = name => {
    const messages = [];
    let message, dispose;
    const view = { visible: true,
      webview: { postMessage: value => messages.push(value),
        onDidReceiveMessage(handler) { message = handler; return { dispose() {} }; } },
      onDidChangeVisibility: () => ({ dispose() {} }),
      onDidDispose(handler) { dispose = handler; return { dispose() {} }; } };
    views.get(`gitpane.${name}`).resolveWebviewView(view);
    return { messages, receive: value => message(value), destroy: () => dispose(),
      snapshot: () => messages.filter(value => value.type === 'snapshot').at(-1) };
  };
  const load = Module._load;
  let changes, graph;
  try {
    Module._load = function(name, ...args) {
      if (name === './webview/html' || name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    changes = lower('changes');
    graph = lower('commitGraph');
  } finally { Module._load = load; }
  const wait = async condition => {
    for (let attempt = 0; attempt < 200 && !condition(); attempt++) await new Promise(resolve => setTimeout(resolve, 10));
    assert.ok(condition(), 'Stacked native views did not reach their expected state');
  };
  const previousErrors = errors.length;
  try {
    await changes.receive({ type: 'ready' });
    await graph.receive({ type: 'ready' });
    const repo = snapshots.at(-1).repositories.find(item => item.name === 'repo');
    const worktree = repo.worktrees.find(item => item.name === 'linked');
    for (const [owner, root] of [[repo, repoPath], [worktree, worktreePath]]) {
      await receive({ type: 'selectOwner', id: owner.id });
      await changes.receive({ type: 'loadChanges', id: owner.id, ownerId: owner.id,
        selectionEpoch: changes.snapshot().selectionEpoch });
      assert.equal(changes.snapshot().selectedOwner.name, path.basename(root));
      const file = changes.snapshot().selectedOwner.groups.flatMap(group => group.files).find(file => file.path.endsWith('.txt'));
      assert.ok(file, 'Native changes must include the selected owner files');
      const before = diffs.length;
      await changes.receive({ type: 'openChange', id: file.id, ownerId: owner.id,
        selectionEpoch: changes.snapshot().selectionEpoch });
      assert.equal(diffs.length, before + 1);
      assert.equal(fs.realpathSync(diffs.at(-1)[1].fsPath), fs.realpathSync(path.join(root, file.path)));
      await wait(() => graph.messages.some(value => value.type === 'graph' &&
        value.repoName === path.basename(root) && !value.loading && value.rows.length > 0));
      const payload = graph.messages.filter(value => value.type === 'graph' && value.repoName === path.basename(root)).at(-1);
      assert.equal(payload.rows[0].message, 'initial');
    }
    assert.equal(errors.length, previousErrors, errors.at(-1));
    console.log('PASS stacked sections show native repository and worktree changes, graphs and diffs');
  } finally {
    changes.destroy();
    graph.destroy();
  }
};
