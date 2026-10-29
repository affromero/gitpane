'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { LinkDocuments } = require('../src/link-documents');
const { openGitlinkDiff } = require('../src/gitlink-diff');

class EventEmitter {
  constructor() { this.listeners = new Set(); this.event = listener => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; }; }
  fire(value) { for (const listener of this.listeners) listener(value); }
  dispose() { this.listeners.clear(); }
}

function uri(value, scheme = 'file') {
  return { fsPath: value, toString: () => `${scheme}:${value}`,
    with: change => uri(change.path || value, change.scheme || scheme) };
}

test('historical pointer documents validate their own ancestor chain', async t => {
  const changes = new EventEmitter();
  const docs = new LinkDocuments({ EventEmitter, workspace: {} }, { onDidChangeTreeData: changes.event }, {
    async validateRepositoryIdentity() { return { valid: true }; }
  });
  t.after(() => docs.dispose());
  const commands = [];
  const vscode = { Uri: { file: uri }, commands: { async executeCommand(...args) { commands.push(args); } } };
  const repo = { rootUri: uri(path.resolve('workspace/child')) };
  const pointer = oid => ({ kind: 'gitlink', oid, mode: 57344 });
  const file = { path: 'nested', status: 'M', gitlink: { old: pointer('1'.repeat(40)), new: pointer('2'.repeat(40)) } };
  let firstCurrent = true;
  const base = { vscode, git: {}, links: docs, repo, repoIdentity: 'a'.repeat(64), repoToken: 1, file,
    oldRef: 'a'.repeat(40), newRef: 'b'.repeat(40), current: () => true, verify: async () => true,
    title: 'nested' };
  await openGitlinkDiff({ ...base, ancestorKey: 'first', verifyAncestors: async () => firstCurrent });
  await openGitlinkDiff({ ...base, ancestorKey: 'second', verifyAncestors: async () => true });
  const first = commands[0][1], second = commands[1][1];
  firstCurrent = false;
  const invalidated = [];
  docs.onDidChange(value => invalidated.push(value.toString()));
  await docs.refresh();
  assert.deepEqual(new Set(invalidated), new Set([commands[0][1].toString(), commands[0][2].toString()]));
  await assert.rejects(docs.provideTextDocumentContent(first), /no longer/i);
  assert.equal(await docs.provideTextDocumentContent(second), `Subproject commit ${'1'.repeat(40)}\n`);
});
