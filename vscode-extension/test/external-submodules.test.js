'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { externalSubmodules } = require('../src/external-submodules');

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-external-child-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const worktree = path.join(root, 'worktree'), child = path.join(worktree, 'child');
  fs.mkdirSync(child, { recursive: true });
  const anchor = 'a'.repeat(64), worktreeIdentity = 'b'.repeat(64), childIdentity = 'c'.repeat(64);
  const directoryIdentity = value => {
    try { const stat = fs.statSync(value, { bigint: true }); return { path: fs.realpathSync.native(value), dev: stat.dev, ino: stat.ino }; }
    catch { return undefined; }
  };
  const tree = { directoryIdentity, backendStatuses: new Map([['parent', { repo_identity: anchor }]]),
    backend: { externalSelections: new Map() }, git: { repositories: [{ rootUri: { fsPath: root, toString: () => 'parent' } }] },
    submoduleDiscovery: { enabled: true } };
  const provider = { tree, ids: new Map(), externalOwners: new Map(), disposed: false,
    idFor(key, live) { live.add(key); if (!this.ids.has(key)) this.ids.set(key, `id-${this.ids.size}`); return this.ids.get(key); },
    resolveWorktree: () => ({ external: true, externalIdentity: worktreeIdentity, repoIdentity: anchor }) };
  const selection = { path: worktree, identity: worktreeIdentity };
  const summary = { repo_identity: 'd'.repeat(64), checkout_submodules: [{ path: 'child', repo: child,
    chain: [{ path: 'child', parent_identity: 'd'.repeat(64), child_identity: childIdentity }],
    status: { repo_identity: childIdentity, is_dirty: true }, submodules: [] }] };
  const options = { item: selection, repoKey: 'parent', repoToken: 1, worktreeId: 'wt', summary,
    live: new Set(), groupsFor: () => [], pageFor: () => ({}), errors: [] };
  const vscode = { workspace: { isTrusted: true }, Uri: { file: value => ({ fsPath: value }) } };
  return { tree, provider, options, vscode, child, root };
}

test('external child owners stay bound to the verified checkout and honor discovery settings', t => {
  const { tree, provider, options, vscode, child, root } = fixture(t);
  const rows = externalSubmodules(provider, vscode, options);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].dirty, true);
  const owner = provider.externalOwners.get(rows[0].id);
  assert.equal(owner.guard(), true);
  assert.equal(tree.backend.externalSelections.get(child).selection.chain[0].path, 'child');
  tree.submoduleDiscovery.enabled = false;
  assert.equal(externalSubmodules(provider, vscode, options)[0].id, rows[0].id);
  fs.renameSync(child, path.join(root, 'old-child'));
  fs.mkdirSync(child);
  assert.equal(owner.guard(), false);
});

test('invalid external hierarchy is reported without creating partial owners', t => {
  const { provider, options, vscode } = fixture(t);
  options.summary.checkout_submodules.push({ ...options.summary.checkout_submodules[0], path: '../escape' });
  assert.deepEqual(externalSubmodules(provider, vscode, options), []);
  assert.match(options.errors[0], /invalid worktree submodule/i);
  assert.equal(provider.externalOwners.size, 0);
});

test('external sibling order follows the selected mode at every depth without changing row IDs', t => {
  const { tree, provider, options, vscode } = fixture(t);
  const add = (name, parent, parentIdentity, chain) => {
    const repo = path.join(parent, name);
    fs.mkdirSync(repo);
    const identity = 'e'.repeat(64);
    return { path: name, repo, status: { repo_identity: identity }, submodules: [],
      chain: [...chain, { path: name, parent_identity: parentIdentity, child_identity: identity }] };
  };
  const children = options.summary.checkout_submodules;
  const child = children[0];
  children.unshift(add('zulu', options.item.path, options.summary.repo_identity, []));
  child.submodules.push(...['beta', 'alpha'].map(name => add(name, child.repo, child.status.repo_identity, child.chain)));
  const ids = new Map();
  for (const order of ['workspace', 'reverse', 'dirty', 'alphabetical']) {
    tree.sortOrder = order;
    const rows = externalSubmodules(provider, vscode, options);
    const nested = rows.find(row => row.name === 'child').submodules;
    assert.deepEqual(rows.map(row => row.name), order === 'reverse' ? ['zulu', 'child'] : ['child', 'zulu']);
    assert.deepEqual(nested.map(row => row.name), order === 'reverse' ? ['beta', 'alpha'] : ['alpha', 'beta']);
    for (const row of [...rows, ...nested]) {
      if (!ids.has(row.name)) ids.set(row.name, row.id);
      assert.equal(row.id, ids.get(row.name));
    }
    children.reverse();
    child.submodules.reverse();
  }
});
