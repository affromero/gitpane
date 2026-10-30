'use strict';

const path = require('node:path');

const KEY = 'gitpane.workspaceRepositories';
const validToken = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const clean = value => value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ').replace(/\$\(/g, '＄(');

function readRepositories(value) {
  if (value === undefined) return [];
  if (!value || value.version !== 1 || !Array.isArray(value.repositories) || value.repositories.length > 128 ||
      Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024 || value.repositories.some(entry => !entry ||
        Object.keys(entry).sort().join(',') !== 'dev,ino,path,repo_identity,source' ||
        ![entry.source, entry.path].every(item => typeof item === 'string' && path.isAbsolute(item) && !item.includes('\0') && item.length <= 4096) ||
        typeof entry.dev !== 'string' || !/^[0-9]+$/.test(entry.dev) || typeof entry.ino !== 'string' || !/^[0-9]+$/.test(entry.ino) ||
        !validToken(entry.repo_identity))) throw new Error('Saved workspace repositories are invalid. Use Manage Workspace Repositories to reset them.');
  if (new Set(value.repositories.map(entry => entry.path)).size !== value.repositories.length) {
    throw new Error('Saved workspace repositories contain duplicates. Use Manage Workspace Repositories to reset them.');
  }
  return structuredClone(value.repositories);
}

class WorkspaceRepositories {
  constructor(tree, backend, git, state, vscode, openings, options = {}) {
    this.tree = tree; this.backend = backend; this.git = git; this.state = state; this.vscode = vscode; this.openings = openings;
    this.version = 0; this.epoch = 0; this.interactive = 0; this.visible = false; this.disposed = false;
    this.pending = Promise.resolve(); this.restoring = false; this.again = false;
    this.retryDelayMs = options.retryDelayMs ?? 5000; this.restoreDelayMs = options.restoreDelayMs ?? 50; this.retryAt = 0;
    try { this.raw = state?.get(KEY); this.repositories = readRepositories(this.raw); }
    catch (error) { this.repositories = []; this.storageError = error.message; this.error = error.message; }
    this.subscription = tree.onDidChangeTreeData(() => this.scheduleRestore());
  }

  enqueue(operation) { const result = this.pending.then(operation); this.pending = result.catch(() => {}); return result; }
  entry(source, identity, repoIdentity) {
    return { source, path: identity.path, dev: String(identity.dev), ino: String(identity.ino), repo_identity: repoIdentity };
  }
  sameSource(entry) {
    const identity = this.tree.directoryIdentity(entry.source);
    return !!identity && identity.path === entry.path && String(identity.dev) === entry.dev && String(identity.ino) === entry.ino &&
      this.tree.isPathInWorkspace(entry.source) && this.tree.isPathInWorkspace(entry.path);
  }
  current(entry, generation, workspaceGeneration) {
    return !this.disposed && generation === this.backend.generation && workspaceGeneration === this.tree.backendGeneration && this.sameSource(entry);
  }
  async validate(entry, generation, workspaceGeneration) {
    if (!this.current(entry, generation, workspaceGeneration)) throw Object.assign(new Error('The selected repository or workspace changed.'), { code: 'stale_target' });
    const result = await this.backend.validateRepositoryIdentity(entry.path, entry.repo_identity);
    if (!this.current(entry, generation, workspaceGeneration) || !result || result.valid !== true) {
      throw Object.assign(new Error('The selected repository or workspace changed.'), { code: 'stale_target' });
    }
  }
  async save(repositories) {
    if (!this.state?.update) throw new Error('VS Code workspace storage is unavailable.');
    const value = { version: 1, repositories };
    if (repositories.length > 128 || Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024) throw new Error('The workspace repository limit was reached.');
    await this.state.update(KEY, value);
  }
  beginAction() { this.interactive++; this.epoch++; if (this.timer) clearTimeout(this.timer); this.timer = undefined; }
  endAction() { this.interactive--; this.signature = undefined; this.scheduleRestore(); }

  async open(entry, guard = () => true) {
    const generation = this.backend.generation, workspaceGeneration = this.tree.backendGeneration;
    const current = () => guard() && this.current(entry, generation, workspaceGeneration);
    await this.validate(entry, generation, workspaceGeneration);
    if (!current()) throw new Error('The selected repository changed.');
    if (this.openings.hasOpening(entry.path)) throw Object.assign(new Error('VS Code is still opening this repository. Try again after it finishes.'), { code: 'open_pending' });
    let repo = this.git.repositories.find(candidate => this.tree.directoryIdentity(candidate.rootUri.fsPath)?.path === entry.path);
    const existingToken = repo && this.tree.repositoryToken(repo);
    const uri = this.vscode.Uri.file(entry.path);
    const revision = this.tree.repositoryRevision(uri);
    if (!repo) {
      try { repo = await this.openings.openTarget(entry.path, uri, current, `workspace:${entry.path}`); }
      catch (error) {
        if (error?.code === 'open_pending') throw error;
        throw Object.assign(new Error(`VS Code could not open ${entry.path}: ${error?.message || String(error)}`), { code: 'open_failed' });
      }
      if (!repo) throw Object.assign(new Error(`VS Code could not open ${entry.path}.`), { code: 'open_failed' });
    }
    const found = repo && this.git.repositories.find(candidate => candidate.rootUri.toString() === repo.rootUri.toString());
    const token = found && this.tree.repositoryToken(found);
    if (!found || !current() || !this.tree.isCurrentRepository(found, token) ||
        (existingToken !== undefined && existingToken !== token) ||
        (existingToken === undefined && this.tree.repositoryRevision(uri) !== revision + 1) ||
        this.tree.directoryIdentity(found.rootUri.fsPath)?.path !== entry.path) throw new Error('The selected repository changed while opening.');
    await this.validate(entry, generation, workspaceGeneration);
    if (!current() || !this.tree.isCurrentRepository(found, token)) throw new Error('The selected repository changed while opening.');
    return found;
  }

  async add() {
    this.beginAction();
    try {
      const generation = this.backend.generation, workspaceGeneration = this.tree.backendGeneration;
      const selected = await this.vscode.window.showOpenDialog({ canSelectFiles: false, canSelectFolders: true,
        canSelectMany: false, openLabel: 'Add Repository', title: 'Choose a repository inside this workspace' });
      if (!selected?.length) return;
      if (selected.length !== 1 || selected[0].scheme !== 'file' || typeof selected[0].fsPath !== 'string' ||
          !path.isAbsolute(selected[0].fsPath) || this.disposed || generation !== this.backend.generation ||
          workspaceGeneration !== this.tree.backendGeneration) throw new Error('The workspace changed. Choose the repository again.');
      const source = selected[0].fsPath, identity = this.tree.directoryIdentity(source);
      if (!identity || !this.tree.isPathInWorkspace(source)) throw new Error('Choose a repository inside the current workspace.');
      const status = await this.backend.previewRepository(identity.path);
      if (!status || !validToken(status.repo_identity)) throw new Error('Gitpane returned invalid repository identity. Update the backend and try again.');
      const entry = this.entry(source, identity, status.repo_identity);
      const current = () => this.current(entry, generation, workspaceGeneration);
      if (!current()) throw new Error('The selected repository or workspace changed.');
      return await this.enqueue(async () => {
        if (this.storageError) throw new Error(this.storageError);
        if (this.repositories.some(item => item.path === entry.path)) return;
        if (this.git.repositories.some(repo => this.tree.directoryIdentity(repo.rootUri.fsPath)?.path === entry.path)) {
          await this.vscode.window.showInformationMessage('This repository is already open in VS Code.'); return;
        }
        const opened = await this.open(entry, current);
        const openedToken = this.tree.repositoryToken(opened);
        if (!current()) throw new Error('The selected repository changed.');
        const next = [...this.repositories, entry];
        await this.save(next);
        try {
          await this.validate(entry, generation, workspaceGeneration);
          if (!current() || !this.tree.isCurrentRepository(opened, openedToken)) throw new Error('The selected repository changed while saving.');
        } catch (error) { await this.save(this.repositories); throw error; }
        this.repositories = next; this.version++; this.error = undefined; this.tree.changed();
      });
    } finally { this.endAction(); }
  }

  isAdded(repo) {
    const identity = this.tree.directoryIdentity(repo?.rootUri?.fsPath);
    const status = repo && this.tree.backendStatuses.get(repo.rootUri.toString());
    return !!identity && this.repositories.some(entry => entry.path === identity.path && entry.dev === String(identity.dev) &&
      entry.ino === String(identity.ino) && entry.repo_identity === status?.repo_identity && this.sameSource(entry));
  }

  async remove(repo, guard = () => true) {
    this.beginAction();
    try { return await this.enqueue(async () => {
      const generation = this.backend.generation, workspaceGeneration = this.tree.backendGeneration;
      const token = this.tree.repositoryToken(repo);
      const current = () => guard() && !this.disposed && generation === this.backend.generation &&
        workspaceGeneration === this.tree.backendGeneration && this.tree.isCurrentRepository(repo, token);
      if (!current() || !this.isAdded(repo)) throw new Error('The selected repository changed.');
      const target = this.tree.directoryIdentity(repo.rootUri.fsPath).path;
      if (this.openings.hasOpening(target)) throw new Error('VS Code is still opening this repository. Try again after it finishes.');
      const next = this.repositories.filter(entry => entry.path !== target);
      await this.save(next);
      if (!current()) { await this.save(this.repositories); throw new Error('The selected repository changed while saving.'); }
      this.repositories = next; this.version++; this.error = undefined; this.tree.changed();
    }); } finally { this.endAction(); }
  }

  async manage() {
    this.beginAction();
    try {
      const version = this.version, generation = this.backend.generation, workspaceGeneration = this.tree.backendGeneration;
      const current = () => !this.disposed && version === this.version && generation === this.backend.generation && workspaceGeneration === this.tree.backendGeneration;
      if (this.storageError) {
        const choice = await this.vscode.window.showWarningMessage('Saved workspace repositories are invalid. Resetting removes their saved additions.',
          { modal: true }, 'Reset Workspace Repositories');
        if (choice !== 'Reset Workspace Repositories') return;
        return await this.enqueue(async () => {
          if (!current()) throw new Error('The workspace changed. Open the list again.');
          await this.save([]);
          if (!current()) { await this.state.update(KEY, this.raw); throw new Error('The workspace changed while saving.'); }
          this.repositories = []; this.storageError = undefined; this.error = undefined; this.version++; this.tree.changed();
        });
      }
      if (!this.repositories.length) { await this.vscode.window.showInformationMessage('There are no saved workspace repositories.'); return; }
      const choices = this.repositories.map(entry => ({ label: clean(path.basename(entry.path)), description: clean(entry.source), entry }));
      const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a saved repository to stop restoring' });
      if (!selected) return;
      if (!choices.includes(selected)) throw new Error('The selected repository changed.');
      return await this.enqueue(async () => {
        if (!current()) throw new Error('The workspace changed. Open the list again.');
        if (this.openings.hasOpening(selected.entry.path)) throw new Error('VS Code is still opening this repository. Try again after it finishes.');
        const next = this.repositories.filter(item => item !== selected.entry);
        await this.save(next);
        if (!current()) { await this.save(this.repositories); throw new Error('The workspace changed while saving.'); }
        this.repositories = next; this.version++; this.error = undefined; this.tree.changed();
      });
    } finally { this.endAction(); }
  }

  setVisible(visible) {
    if (this.disposed || this.visible === visible) return;
    this.visible = visible; this.epoch++; this.signature = undefined;
    if (visible) this.retryAt = 0;
    if (!visible && this.timer) clearTimeout(this.timer);
    this.timer = undefined; this.scheduleRestore();
  }
  scheduleRestore() {
    if (this.disposed || !this.visible || this.interactive || this.storageError || !this.repositories.length) return;
    const signature = JSON.stringify([this.version, this.backend.generation, this.tree.backendGeneration,
      this.git.repositories.map(repo => [repo.rootUri.toString(), this.tree.repositoryToken(repo)])]);
    if (signature === this.signature) return;
    this.signature = signature;
    if (this.restoring) { this.again = true; return; }
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = undefined; this.restoring = true;
      const epoch = this.epoch, version = this.version;
      const current = () => !this.disposed && this.visible && !this.interactive && this.epoch === epoch && this.version === version;
      let retry = false;
      (async () => {
        const errors = [];
        for (const entry of [...this.repositories]) {
          if (!current()) return;
          try { await this.open(entry, current); }
          catch (error) {
            if (!current()) return;
            errors.push(error.message);
            if (!['open_pending', 'open_failed', 'repository_error', 'outside_workspace', 'stale_target'].includes(error.code)) break;
          }
        }
        if (current()) {
          retry = !!errors.length;
          this.retryAt = retry ? Date.now() + this.retryDelayMs : 0;
          this.error = retry ? `Could not restore workspace repository: ${errors.join('; ')}` : undefined;
          this.tree.changed();
        }
      })().catch(error => { if (current()) { retry = true; this.retryAt = Date.now() + this.retryDelayMs; this.error = error.message; this.tree.changed(); } }).finally(() => {
        this.restoring = false;
        if (this.again || (retry && current())) { this.again = false; this.signature = undefined; this.scheduleRestore(); }
      });
    }, Math.max(this.restoreDelayMs, this.retryAt - Date.now()));
  }
  dispose() { this.disposed = true; this.epoch++; if (this.timer) clearTimeout(this.timer); this.subscription.dispose(); }
}

module.exports = { WorkspaceRepositories, readRepositories };
