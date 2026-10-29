'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { activatedProvider } = require('./integration/host');

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-host-capture-'));
  const source = path.join(root, 'src');
  fs.mkdirSync(source);
  const modulePath = path.join(source, 'extension.js');
  fs.writeFileSync(modulePath, 'module.exports = { GitpaneViewProvider: class { refresh(value) { this.value = value; } } };\n');
  t.after(() => { delete require.cache[require.resolve(modulePath)]; fs.rmSync(root, { recursive: true, force: true }); });
  const { GitpaneViewProvider } = require(modulePath);
  return { root, GitpaneViewProvider, original: Object.getOwnPropertyDescriptor(GitpaneViewProvider.prototype, 'refresh') };
}

test('host harness captures the command-owned provider and restores its refresh method', async t => {
  const { root, GitpaneViewProvider, original } = fixture(t);
  const provider = new GitpaneViewProvider();
  const vscode = {
    extensions: { getExtension: () => ({ activate: async () => {} }) },
    commands: { executeCommand: async name => {
      assert.equal(name, 'gitpane.refresh');
      await provider.refresh(true);
    } }
  };
  assert.equal(await activatedProvider(vscode, root), provider);
  assert.equal(provider.value, true);
  assert.deepEqual(Object.getOwnPropertyDescriptor(GitpaneViewProvider.prototype, 'refresh'), original);
});

test('host harness restores refresh if extension activation fails', async t => {
  const { root, GitpaneViewProvider, original } = fixture(t);
  const vscode = { extensions: { getExtension: () => ({ activate: async () => { throw new Error('activation failed'); } }) } };
  await assert.rejects(activatedProvider(vscode, root), /activation failed/);
  assert.deepEqual(Object.getOwnPropertyDescriptor(GitpaneViewProvider.prototype, 'refresh'), original);
});
