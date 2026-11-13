'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { ComparisonController } = require('../src/comparison-controller');
const { ComparisonModel } = require('../src/comparison-model');

async function modelFor(first, second, identity, file, current) {
  const service = { cancel() {}, async request(method, _repo, params) {
    if (method === 'resolve_revision') return { oid: params.revision, repo_identity: identity };
    return { base_oid: first, to_oid: second, repo_identity: identity, total: 1, next: null, files: [file] };
  } };
  const model = new ComparisonModel(service, '/repo', current);
  await model.set('from', first);
  await model.set('to', second);
  return model;
}

test('renamed comparison files open the pinned old and new paths', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'c'.repeat(64);
  const file = { status: 'R', path: 'new.txt', old_path: 'old.txt', is_submodule: false };
  const opened = [], documents = [];
  let authorized = true;
  const model = await modelFor(first, second, identity, file, () => authorized);
  const uri = fsPath => ({ fsPath, with: change => ({ fsPath, scheme: change.scheme }) });
  const vscode = { Uri: { file: uri }, commands: { async executeCommand(...args) { opened.push(args); } } };
  const links = { createLiveBinary(target, _metadata, read) { documents.push({ target, read }); return target; } };
  const backend = { async gitSideSession(_repo, options) {
    assert.equal(options.literalObjects, true);
    return { git: { key: 'git' } };
  }, gitSideCurrent: () => true, async gitSideOperation(_session, _identity, operation) {
    return Buffer.from(`${operation.ref}:${operation.path}`);
  } };
  const controller = new ComparisonController({ vscode, model, backend, links,
    tree: {}, repo: { rootUri: uri('/repo') }, token: 1, current: () => authorized, validate: async () => true });
  const files = await controller.compare();
  await controller.open(files[0].id);
  assert.equal(opened[0][0], 'vscode.diff');
  assert.equal(opened[0][1].fsPath, path.join('/repo', 'old.txt'));
  assert.equal(opened[0][2].fsPath, path.join('/repo', 'new.txt'));
  assert.equal((await documents[0].read()).toString(), `${first}:old.txt`);
  assert.equal((await documents[1].read()).toString(), `${second}:new.txt`);
  authorized = false;
  await assert.rejects(documents[1].read(), /no longer in the workspace/i);
  controller.clearFiles();
  await assert.rejects(controller.open(files[0].id), /no longer available/i);
});

test('hiding a graph while a comparison diff is opening prevents the stale action', async () => {
  const first = 'a'.repeat(40), second = 'b'.repeat(40), identity = 'c'.repeat(64);
  const file = { status: 'M', path: 'file.txt', is_submodule: false };
  let release, entered, visible = true;
  const started = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const opened = [];
  const uri = fsPath => ({ fsPath, with: change => ({ fsPath, scheme: change.scheme }) });
  const model = await modelFor(first, second, identity, file, () => true);
  const backend = { async gitSideSession() { entered(); await held; return { git: { key: 'git' } }; },
    gitSideCurrent: () => true, async gitSideOperation() { return Buffer.from(''); } };
  const controller = new ComparisonController({ vscode: { Uri: { file: uri }, commands: { executeCommand: (...args) => opened.push(args) } },
    model, backend, links: { createLiveBinary: target => target }, tree: {}, repo: { rootUri: uri('/repo') }, token: 1,
    current: () => true, validate: async () => true, actionGuard: () => () => visible });
  const files = await controller.compare();
  const action = controller.open(files[0].id);
  await started;
  visible = false;
  controller.suspend();
  release();
  await assert.rejects(action, /no longer available/i);
  assert.deepEqual(opened, []);
});
