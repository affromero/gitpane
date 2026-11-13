'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const Module = require('node:module');

module.exports = async function compareExternal({ vscode, provider, documents, diffs, errors, gitAt, siblingPath, selected, pointerOld, pointerNew }) {
  let receive, input, disposed = false;
  const messages = [], disposals = [];
  const panel = { visible: true, webview: { postMessage: message => messages.push(message),
    onDidReceiveMessage(handler) { receive = handler; return { dispose() {} }; } },
    onDidDispose(handler) { disposals.push(handler); return { dispose() {} }; },
    onDidChangeViewState() { return { dispose() {} }; }, reveal() {},
    dispose() { if (!disposed) { disposed = true; for (const handler of disposals) handler(); } } };
  const originalPanel = vscode.window.createWebviewPanel, originalInput = vscode.window.showInputBox;
  const originalColumn = vscode.ViewColumn, load = Module._load;
  vscode.window.createWebviewPanel = () => panel;
  vscode.window.showInputBox = async () => input;
  vscode.ViewColumn = { Active: 1 };
  try {
    const menu = await provider.actions.rowMenu('worktree', selected.id);
    assert.ok(menu?.choices.some(choice => choice.action === 'compareWorktree'), 'outside worktree offers comparison before inspection');
    Module._load = function(name, ...args) {
      if (name === './graph/html') return { getHtml: () => '<html></html>' };
      return load.call(this, name, ...args);
    };
    await provider.actions.rowMenuAction('worktree', selected.id, menu.token, 'compareWorktree');
    Module._load = load;
    assert.ok(receive, errors.at(-1));
    await receive({ type: 'ready' });
    input = gitAt(siblingPath, 'rev-parse', 'HEAD^');
    await receive({ type: 'comparisonPick', which: 'from' });
    input = gitAt(siblingPath, 'rev-parse', 'HEAD');
    await receive({ type: 'comparisonPick', which: 'to' });
    const state = messages.findLast(message => message.type === 'comparisonState' && !message.loading);
    assert.equal(state?.dirty, true, state?.error);
    assert.equal(state.head, 'feature/sibling');
    const file = state.files.find(item => item.path === 'external-tracked.txt');
    assert.ok(file, state.error || 'comparison must include the committed external file');
    const before = diffs.length;
    await receive({ type: 'openComparisonFile', id: file.id });
    assert.equal(diffs.length, before + 1, errors.at(-1));
    const comparisonDocument = diffs.at(-1)[1];
    assert.equal((await documents.get('gitpane-blob').readFile(comparisonDocument)).toString(), 'committed\n');
    const pointer = state.files.find(item => item.path === 'external-child' && item.is_submodule);
    assert.ok(pointer, 'comparison must include the changed submodule pointer');
    await receive({ type: 'openComparisonFile', id: pointer.id });
    assert.equal(diffs.length, before + 2, errors.at(-1));
    const pointers = [diffs.at(-1)[0], diffs.at(-1)[1]];
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointers[0]), `Subproject commit ${pointerOld}\n`);
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointers[1]), `Subproject commit ${pointerNew}\n`);
    const originalVisibility = provider.view.visible;
    provider.view.visible = false;
    const tracked = path.join(siblingPath, 'external-tracked.txt');
    const untracked = path.join(siblingPath, 'external-untracked.txt');
    const waitForDirty = async expected => {
      const deadline = Date.now() + 20000;
      while (messages.findLast(message => message.type === 'comparisonState')?.dirty !== expected) {
        if (Date.now() >= deadline) throw new Error(`Outside worktree status did not become ${expected ? 'dirty' : 'clean'}.`);
        await new Promise(resolve => setTimeout(resolve, 100));
      }
    };
    try {
      gitAt(siblingPath, 'restore', '--staged', 'external-tracked.txt');
      fs.writeFileSync(tracked, 'committed\n');
      fs.unlinkSync(untracked);
      await waitForDirty(false);
      fs.writeFileSync(tracked, 'staged\n');
      gitAt(siblingPath, 'add', '--', 'external-tracked.txt');
      fs.writeFileSync(tracked, 'working\n');
      fs.writeFileSync(untracked, 'new file\n');
      await waitForDirty(true);
    } finally {
      provider.view.visible = originalVisibility;
      fs.writeFileSync(tracked, 'staged\n');
      gitAt(siblingPath, 'add', '--', 'external-tracked.txt');
      fs.writeFileSync(tracked, 'working\n');
      fs.writeFileSync(untracked, 'new file\n');
    }
    panel.dispose();
    assert.equal((await documents.get('gitpane-blob').readFile(comparisonDocument)).toString(), 'committed\n');
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointers[0]), `Subproject commit ${pointerOld}\n`);
    assert.equal(await documents.get('gitpane-link').provideTextDocumentContent(pointers[1]), `Subproject commit ${pointerNew}\n`);
    console.log('PASS outside worktree comparison preserves its context and committed content');
    return { file: comparisonDocument, pointers };
  } finally {
    Module._load = load;
    panel.dispose();
    vscode.window.createWebviewPanel = originalPanel;
    vscode.window.showInputBox = originalInput;
    vscode.ViewColumn = originalColumn;
  }
};
