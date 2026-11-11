'use strict';

const vscode = require('vscode');
const { isStashOid, openStashDiff, stashErrorMessage, stashFileLabel, stashUnavailableMessage,
  validateStashContent, validateStashList } = require('./stash-preview');

// Stash identity is opaque to the webview: node ids map to the repository and
// the immutable stash OID here. `index` is display only, so renumbering can
// never redirect a request at a different stash.
class StashView {
  constructor(provider, source) {
    this.provider = provider;
    this.source = source;
    this.entries = new Map();
    this.files = new Map();
    this.live = new Set();
    this.nextFileId = 0;
  }

  snapshot(repo, repoKey, repoToken, stashes, live, idFor) {
    const payload = [];
    for (const item of Array.isArray(stashes) ? stashes : []) {
      if (!item || !Number.isSafeInteger(item.index) || item.index < 0 ||
          typeof item.message !== 'string' || typeof item.oid !== 'string') continue;
      const id = idFor(JSON.stringify(['stash', repoKey, repoToken, item.oid, item.index]), live);
      this.live.add(id);
      this.entries.set(id, { repoKey, repoToken, oid: item.oid, index: item.index, message: item.message, repo });
      const listing = this.files.get(id);
      const loaded = listing?.oid === item.oid ? listing : undefined;
      payload.push({
        id, index: item.index, message: item.message,
        ...(loaded ? {
          fileState: loaded.phase,
          ...(loaded.error ? { fileError: loaded.error } : {}),
          files: loaded.phase === 'ready' ? loaded.files.map(file => ({
            id: file.id, status: file.status, path: file.path, oldPath: file.old_path,
            label: stashFileLabel(file), binary: file.binary, gitlink: file.gitlink, source: file.source
          })) : []
        } : {})
      });
    }
    return payload;
  }

  // Called once per unpublished snapshot, after every repository was visited.
  prune() {
    for (const id of this.entries.keys()) {
      if (this.live.has(id)) continue;
      this.entries.delete(id);
      this.files.delete(id);
    }
    this.live.clear();
  }

  current(id) {
    const entry = this.entries.get(id);
    if (!entry || this.provider.disposed) return undefined;
    const repo = this.provider.tree.git.repositories.find(item => item.rootUri.toString() === entry.repoKey);
    if (!repo || !this.provider.tree.isCurrentRepository(repo, entry.repoToken)) return undefined;
    const status = this.provider.tree.backendStatuses.get(entry.repoKey);
    if (!/^[a-f0-9]{64}$/.test(status?.repo_identity || '')) return undefined;
    if (!(status.stashes || []).some(item => item && item.oid === entry.oid && item.index === entry.index)) return undefined;
    return { ...entry, repo, identity: status.repo_identity };
  }

  isCurrent(id, oid) {
    const entry = this.entries.get(id);
    if (!entry || entry.oid !== oid) return false;
    const repo = this.provider.tree.git.repositories.find(item => item.rootUri.toString() === entry.repoKey);
    if (!repo || !this.provider.tree.isCurrentRepository(repo, entry.repoToken)) return false;
    const status = this.provider.tree.backendStatuses.get(entry.repoKey);
    return (status?.stashes || []).some(item => item && item.oid === oid && item.index === entry.index);
  }

  async load(id) {
    if (this.provider.disposed) return;
    const stash = this.current(id);
    if (!stash || !isStashOid(stash.oid)) return;
    const cached = this.files.get(id);
    if (cached?.oid === stash.oid) {
      if (cached.pending) return cached.pending;
      if (cached.phase === 'ready' || cached.phase === 'empty') return;
    }
    const generation = this.provider.tree.backendGeneration;
    const state = { oid: stash.oid, phase: 'loading', error: undefined, files: [] };
    this.files.set(id, state);
    const current = () => !this.provider.disposed && this.provider.tree.backendGeneration === generation &&
      this.files.get(id) === state && this.isCurrent(id, stash.oid);
    this.provider.publish();
    const pending = (async () => {
      try {
        const list = validateStashList(await this.source.listFiles({
          repo: stash.repo.rootUri.fsPath, identity: stash.identity, oid: stash.oid }), stash.oid, stash.identity);
        if (!current()) return;
        state.files = list.files.map(file => ({ ...file, id: `stash-file-${++this.nextFileId}` }));
        state.phase = state.files.length ? 'ready' : 'empty';
        state.error = undefined;
      } catch (error) {
        if (!current()) return;
        state.phase = 'error';
        state.files = [];
        state.error = stashErrorMessage(error);
      } finally {
        delete state.pending;
        if (current()) this.provider.publish();
      }
    })();
    state.pending = pending;
    return pending;
  }

  async open(id, fileId) {
    const provider = this.provider;
    const stash = this.current(id);
    if (!stash) throw new Error('The selected stash is no longer available. Refresh and try again.');
    const state = this.files.get(id);
    const file = state?.oid === stash.oid && Array.isArray(state.files) ?
      state.files.find(item => item.id === fileId) : undefined;
    if (!file) throw new Error('The saved stash file is no longer available. Refresh and try again.');
    const unavailable = stashUnavailableMessage(file);
    if (unavailable) {
      await vscode.window.showInformationMessage(unavailable);
      return;
    }
    if (!provider.links) throw new Error('Gitpane could not open the stash diff. Reload the window and try again.');
    const generation = provider.tree.backendGeneration;
    const current = () => !provider.disposed && provider.tree.backendGeneration === generation &&
      this.isCurrent(id, stash.oid) && this.files.get(id)?.oid === stash.oid;
    let contents;
    try {
      contents = validateStashContent(await provider.stash.readFile({
        repo: stash.repo.rootUri.fsPath, identity: stash.identity, oid: stash.oid, file }));
    } catch (error) { throw new Error(stashErrorMessage(error)); }
    if (!current()) throw new Error('The selected stash is no longer available. Refresh and try again.');
    const binary = [contents.old, contents.new].find(side => side?.kind === 'binary');
    if (binary) {
      await vscode.window.showInformationMessage(`Gitpane cannot show a text diff for the saved binary file ${file.path}.`);
      return;
    }
    await openStashDiff({ vscode, links: provider.links, repo: stash.repo, repoToken: stash.repoToken,
      identity: stash.identity, oid: stash.oid, index: stash.index, file, contents, current });
  }

  dispose() {
    this.entries.clear();
    this.files.clear();
    this.live.clear();
  }
}

module.exports = { StashView };
