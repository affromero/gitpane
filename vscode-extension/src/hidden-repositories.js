'use strict';

const path = require('node:path');

const KEY = 'gitpane.hiddenRepositories';
const validToken = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const clean = value => value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ').replace(/\$\(/g, '＄(');

function readHidden(value) {
  if (value === undefined) return [];
  if (!value || value.version !== 1 || !Array.isArray(value.repositories) || value.repositories.length > 1024 ||
      Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024 || value.repositories.some(item => !item ||
        Object.keys(item).sort().join(',') !== 'dev,ino,path,repo_identity' || typeof item.path !== 'string' ||
        !path.isAbsolute(item.path) || item.path.includes('\0') || item.path.length > 4096 ||
        typeof item.dev !== 'string' || typeof item.ino !== 'string' ||
        !/^[0-9]+$/.test(item.dev) || !/^[0-9]+$/.test(item.ino) || !validToken(item.repo_identity))) {
    throw new Error('Saved hidden repositories are invalid. Use Restore Hidden Repositories to reset them.');
  }
  if (new Set(value.repositories.map(item => JSON.stringify(item))).size !== value.repositories.length) {
    throw new Error('Saved hidden repositories contain duplicates. Use Restore Hidden Repositories to reset them.');
  }
  return structuredClone(value.repositories);
}

class HiddenRepositories {
  constructor(tree, backend, state, vscode) {
    this.tree = tree;
    this.backend = backend;
    this.state = state;
    this.vscode = vscode;
    this.pending = Promise.resolve();
    this.version = 0;
    this.disposed = false;
    try { this.raw = state?.get(KEY); this.repositories = readHidden(this.raw); }
    catch (error) { this.repositories = []; this.error = error.message; this.storageError = error.message; }
  }

  entry(identity, repoIdentity) {
    if (!identity || !validToken(repoIdentity)) return undefined;
    return { path: identity.path, dev: String(identity.dev), ino: String(identity.ino), repo_identity: repoIdentity };
  }

  isHidden(identity, repoIdentity) {
    if (!identity || (repoIdentity !== undefined && !validToken(repoIdentity))) return false;
    return this.repositories.some(item => item.path === identity.path && item.dev === String(identity.dev) &&
      item.ino === String(identity.ino) && (repoIdentity === undefined || item.repo_identity === repoIdentity));
  }

  enqueue(operation) {
    const result = this.pending.then(operation);
    this.pending = result.catch(() => {});
    return result;
  }

  async save(repositories) {
    if (!this.state?.update) throw new Error('VS Code workspace storage is unavailable.');
    const value = { version: 1, repositories };
    if (repositories.length > 1024 || Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024) {
      throw new Error('The hidden repository storage limit was reached.');
    }
    await this.state.update(KEY, value);
  }

  hide(target, repoIdentity, guard) {
    return this.enqueue(async () => {
      if (this.storageError) throw new Error(this.storageError);
      const generation = this.backend.generation;
      const workspaceGeneration = this.tree.backendGeneration;
      const entry = this.entry(this.tree.directoryIdentity(target), repoIdentity);
      const current = () => !this.disposed && this.backend.generation === generation && this.tree.backendGeneration === workspaceGeneration && guard() &&
        this.tree.isPathInWorkspace(target) && JSON.stringify(this.entry(this.tree.directoryIdentity(target), repoIdentity)) === JSON.stringify(entry);
      if (!entry || !current()) throw new Error('The selected repository changed. Refresh and try again.');
      if (this.isHidden(this.tree.directoryIdentity(target), repoIdentity)) return;
      const next = [...this.repositories, entry];
      await this.save(next);
      if (!current()) { await this.save(this.repositories); throw new Error('The selected repository changed while saving.'); }
      this.repositories = next;
      this.version++;
      this.tree.changed();
    });
  }

  async manage() {
    const version = this.version;
    const generation = this.backend.generation;
    const workspaceGeneration = this.tree.backendGeneration;
    const current = () => !this.disposed && this.version === version && this.backend.generation === generation &&
      this.tree.backendGeneration === workspaceGeneration;
    if (this.storageError) {
      const choice = await this.vscode.window.showWarningMessage('Saved hidden repositories are invalid. Resetting makes all repositories visible.',
        { modal: true }, 'Reset Hidden Repositories');
      if (choice !== 'Reset Hidden Repositories') return;
      return this.enqueue(async () => {
        if (!current()) throw new Error('The workspace changed. Open the hidden repository list again.');
        await this.save([]);
        if (!current()) { await this.state.update(KEY, this.raw); throw new Error('The workspace changed while resetting.'); }
        this.repositories = [];
        this.storageError = undefined;
        this.error = undefined;
        this.version++;
        this.tree.changed();
      });
    }
    if (!this.repositories.length) { await this.vscode.window.showInformationMessage('There are no hidden repositories.'); return; }
    const choices = this.repositories.map(item => ({ label: clean(path.basename(item.path)), description: clean(item.path), item }));
    const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a repository to restore', matchOnDescription: true });
    if (!selected) return;
    if (!choices.includes(selected)) throw new Error('The selected hidden repository changed.');
    return this.enqueue(async () => {
      if (!current()) throw new Error('The workspace changed. Open the hidden repository list again.');
      const next = this.repositories.filter(item => item !== selected.item);
      await this.save(next);
      if (!current()) { await this.save(this.repositories); throw new Error('The workspace changed while restoring.'); }
      this.repositories = next;
      this.version++;
      this.tree.changed();
    });
  }

  dispose() { this.disposed = true; }
}

module.exports = { HiddenRepositories, readHidden };
