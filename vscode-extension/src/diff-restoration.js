'use strict';

const { validRecord } = require('./change-pager');
const { richComparison } = require('./model');
const { createLiveGitSide } = require('./git-side-documents');
const fs = require('node:fs');
const path = require('node:path');

const KEY = 'gitpane.diffRestoration';
const supported = new Set(['working_modified', 'working_deleted', 'index_modified', 'index_added', 'index_deleted']);
const scopeKey = snapshot => JSON.stringify([snapshot.roots, snapshot.rootSources, snapshot.rootIdentities]);

class DiffRestoration {
  constructor(vscode, tree, backend, links, state) {
    this.vscode = vscode;
    this.tree = tree;
    this.backend = backend;
    this.links = links;
    this.state = state;
    this.writes = Promise.resolve();
    this.pending = new Map();
    this.closed = new Set();
  }

  entries() {
    const value = this.state.get(KEY);
    if (value === undefined) return {};
    if (value?.version !== 1 || !value.entries || typeof value.entries !== 'object' || Array.isArray(value.entries)) {
      throw new Error('Gitpane diff restoration data is invalid or from an unsupported version.');
    }
    return value.entries;
  }

  update(action) {
    const next = this.writes.then(async () => {
      const entries = { ...this.entries() };
      action(entries);
      await this.state.update(KEY, { version: 1, entries });
    });
    this.writes = next.catch(() => {});
    return next;
  }

  current(repo, token, identity, snapshot) {
    const key = repo.rootUri.toString();
    return this.vscode.workspace.isTrusted === true && this.tree.isCurrentRepository(repo, token) &&
      !this.tree.submoduleHierarchy?.edges.has(key) &&
      this.independent(repo.rootUri.fsPath) &&
      this.tree.backendStatuses.get(key)?.repo_identity === identity &&
      !this.tree.backendStatuses.get(key)?.is_linked_worktree &&
      this.backend.isCurrentMutation(snapshot, repo.rootUri.fsPath);
  }

  independent(repoPath) {
    let parent;
    try { parent = path.dirname(fs.realpathSync.native(repoPath)); } catch { return false; }
    while (true) {
      try { fs.lstatSync(path.join(parent, '.git')); return false; }
      catch (error) { if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') return false; }
      const next = path.dirname(parent);
      if (next === parent) return true;
      parent = next;
    }
  }

  async remember(uri, repo, token, identity, record, which, guard, mode) {
    if (this.vscode.workspace.isTrusted !== true || !validRecord(record) || record.is_submodule ||
        !supported.has(record.kind) || !['left', 'right'].includes(which) || ![0o100644, 0o100664, 0o100755].includes(mode)) return;
    const snapshot = await this.backend.mutationSnapshot();
    if (!guard() || !this.current(repo, token, identity, snapshot)) return;
    const side = richComparison(record)[which];
    if (!side?.gitPath) return;
    const key = uri.toString();
    this.closed.delete(key);
    const descriptor = { repo: repo.rootUri.fsPath, identity, record, which, scope: scopeKey(snapshot) };
    await this.update(entries => {
      if (this.closed.has(key)) return;
      if (!Object.hasOwn(entries, key) && Object.keys(entries).length >= 256 && this.vscode.window?.tabGroups) {
        const open = new Set([...this.links.documents.keys(), ...this.pending.keys()]);
        for (const group of this.vscode.window.tabGroups.all) for (const tab of group.tabs) {
          for (const item of [tab.input?.uri, tab.input?.original, tab.input?.modified]) if (item) open.add(item.toString());
        }
        for (const old of Object.keys(entries)) if (!open.has(old)) delete entries[old];
      }
      if (!Object.hasOwn(entries, key) && Object.keys(entries).length >= 256) throw new Error('Too many restorable Gitpane diffs are open.');
      entries[key] = descriptor;
    });
    if (this.closed.has(key) || !guard() || !this.current(repo, token, identity, snapshot)) throw new Error('The Gitpane diff is no longer in the workspace.');
  }

  restore(uri) {
    const key = uri.toString();
    if (this.pending.has(key)) return this.pending.get(key);
    const pending = this.restoreOnce(uri).finally(() => this.pending.delete(key));
    this.pending.set(key, pending);
    return pending;
  }

  async restoreOnce(uri) {
    await this.writes;
    const descriptor = this.entries()[uri.toString()];
    if (!descriptor || uri.scheme !== 'gitpane-blob' || typeof descriptor.repo !== 'string' ||
        !/^[a-f0-9]{64}$/.test(descriptor.identity || '') || !validRecord(descriptor.record) ||
        descriptor.record.is_submodule || !supported.has(descriptor.record.kind) ||
        !['left', 'right'].includes(descriptor.which) || typeof descriptor.scope !== 'string') return false;
    const repo = this.tree.git.repositories.find(item => item.rootUri.fsPath === descriptor.repo);
    if (!repo) return false;
    const token = this.tree.repositoryToken(repo);
    const snapshot = await this.backend.mutationSnapshot();
    const current = () => !this.closed.has(uri.toString()) && this.current(repo, token, descriptor.identity, snapshot) &&
      JSON.stringify(this.entries()[uri.toString()]) === JSON.stringify(descriptor);
    if (!current() || scopeKey(snapshot) !== descriptor.scope) return false;
    const revision = this.tree.contentRevision(repo);
    const result = await this.backend.verifyChanges(descriptor.repo, descriptor.record);
    await this.writes;
    if (!current() || revision !== this.tree.contentRevision(repo) || !validRecord(result?.change) || JSON.stringify(result.change) !== JSON.stringify(descriptor.record)) return false;
    const side = richComparison(descriptor.record)[descriptor.which];
    if (!side?.gitPath) return false;
    await createLiveGitSide({ vscode: this.vscode, links: this.links, backend: this.backend, repo,
      repoIdentity: descriptor.identity, repoToken: token, side, current, restoreUri: uri, requireRegular: true,
      revision: () => this.tree.contentRevision(repo) });
    if (current() && revision === this.tree.contentRevision(repo)) return true;
    this.links.remove(uri.toString());
    return false;
  }

  forget(key) {
    this.closed.add(key);
    return this.update(entries => { delete entries[key]; }).then(() => { this.closed.delete(key); });
  }
}

module.exports = { DiffRestoration };
