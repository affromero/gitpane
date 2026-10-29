'use strict';

const { randomUUID } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

function statusRevision(getStatus, watchedPath, getHead) {
  let observed, watchError, watchToken, suspended = false;
  const update = () => {
    const status = getStatus();
    if (suspended || (watchToken !== undefined && status?.watchTokens?.get(watchedPath) !== watchToken)) return status;
    if (status?.watchedPaths?.includes(watchedPath)) {
      observed = status.files.find(file => file.path === watchedPath) ?? null;
      watchError = status.watched_errors?.find(error => error.path === watchedPath);
    }
    return status;
  };
  const revision = () => {
    const status = update();
    return JSON.stringify([status?.head_oid, status?.branch, observed, watchError, getHead?.()]);
  };
  revision.error = () => { update(); return watchError; };
  revision.suspend = () => { suspended = true; };
  revision.activate = token => { watchToken = token; observed = undefined; watchError = undefined; suspended = false; };
  return revision;
}

// A Git symlink stores the link text, not the contents of the linked file.
// Keep paths and guards in the extension host, never in the document URI.
class LinkDocuments {
  constructor(vscode, tree, backend) {
    this.vscode = vscode;
    this.backend = backend;
    this.output = tree.output;
    this.documents = new Map();
    this.nextWatchToken = 0;
    this.disposed = false;
    this.activeReads = 0;
    this.identityChecks = new Map();
    this.waiters = [];
    this.refreshTimer = undefined;
    this.refreshing = false;
    this.refreshAgain = false;
    this.emitter = new vscode.EventEmitter();
    this.onDidChange = this.emitter.event;
    this.subscriptions = [
      tree.onDidChangeTreeData(() => this.scheduleRefresh()),
      vscode.workspace.onDidOpenTextDocument?.(document => {
        const entry = this.documents.get(document.uri.toString());
        if (entry) { clearTimeout(entry.timer); entry.timer = undefined; }
      }) || { dispose() {} },
      vscode.workspace.onDidCloseTextDocument?.(document => this.remove(document.uri.toString())) || { dispose() {} }
    ];
  }

  async read(entry) {
    if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
    if (entry.content !== undefined) return entry.content;
    await this.acquire();
    try {
      if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
      const result = await this.backend.readWorkingSymlink(entry.repo, entry.path);
      if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
      if (entry.verify && !await entry.verify()) throw new Error('The symlink change is no longer in the workspace.');
      if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
      if (!result || typeof result.target !== 'string') throw new Error('Gitpane returned invalid symlink contents.');
      return result.target;
    } finally { this.release(); }
  }

  async current(entry) {
    if (this.disposed || entry.closed) return false;
    const current = await entry.guard();
    if (this.disposed || entry.closed || !current) return false;
    const watchError = entry.revision?.error?.();
    if (watchError) {
      const error = new Error(`Gitpane could not read symlink ${entry.path}: ${watchError.message}`);
      error.code = 'watched_path_error';
      throw error;
    }
    return true;
  }

  acquire() {
    if (this.disposed) return Promise.reject(new Error('The symlink diff is no longer available.'));
    if (this.activeReads < 2) { this.activeReads++; return Promise.resolve(); }
    return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
  }

  release() {
    const next = this.waiters.shift();
    if (next) next.resolve();
    else this.activeReads--;
  }

  fingerprint(entry, target) {
    if (entry.content !== undefined) return 'static';
    let identity;
    try {
      const stat = fs.lstatSync(path.join(entry.repo, entry.path), { bigint: true });
      identity = [stat.dev, stat.ino, stat.mode, stat.size, stat.mtimeNs, stat.ctimeNs].join(':');
    } catch (error) { identity = `error:${error.code || error.message}`; }
    let revision;
    try { revision = entry.revision?.(); }
    catch (error) { revision = `error:${error.message}`; }
    return JSON.stringify([identity, revision ?? null, target]);
  }

  async probe(repo, relative) {
    await this.acquire();
    try {
      if (this.disposed) throw new Error('The symlink diff is no longer available.');
      const result = await this.backend.readWorkingSymlink(repo, relative);
      if (!result || typeof result.target !== 'string') throw new Error('Gitpane returned invalid symlink contents.');
      return result.target;
    } finally { this.release(); }
  }

  scheduleRefresh() {
    if (this.disposed || this.refreshTimer !== undefined) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined;
      this.refresh().catch(error => {
        const message = `Gitpane could not refresh a symlink diff: ${error.message}`;
        if (this.output) this.output.appendLine(message);
        else this.vscode.window?.showErrorMessage(message);
      });
    }, 50);
  }

  async refresh() {
    if (this.refreshing) { this.refreshAgain = true; return; }
    this.refreshing = true;
    try {
      const checks = new Map(), probes = new Map();
      for (const entry of this.documents.values()) {
        let current, failure;
        try {
          if (entry.content !== undefined && entry.group) {
            if (!checks.has(entry.group)) checks.set(entry.group, this.current(entry));
            current = await checks.get(entry.group) && (!entry.guardSync || entry.guardSync());
          } else current = await this.current(entry);
        }
        catch (error) { current = false; failure = error; }
        let target;
        if (current && entry.content === undefined) {
          const key = JSON.stringify([this.repoKey(entry.repo), entry.path]);
          try {
            if (!probes.has(key)) probes.set(key, this.probe(entry.repo, entry.path));
            target = await probes.get(key);
            current = await this.current(entry);
          } catch (error) { current = false; failure = error; }
        }
        if (this.disposed || entry.closed) continue;
        const next = current ? this.fingerprint(entry, target) : `stale:${failure?.message || ''}`;
        if (entry.fingerprint !== next) {
          entry.fingerprint = next;
          if (failure?.code === 'watched_path_error') this.vscode.window?.showErrorMessage(failure.message);
          this.emitter.fire(entry.uri);
        }
      }
    } finally {
      this.refreshing = false;
      if (this.refreshAgain && !this.disposed) { this.refreshAgain = false; this.scheduleRefresh(); }
    }
  }

  async create(fileUri, repo, relative, guard, verify, revision) {
    if (this.disposed || this.documents.size >= 256) throw new Error('Too many symlink diffs are open.');
    const entry = { repo, path: relative, guard, verify, revision, closed: false };
    revision?.suspend?.();
    const initialContent = await this.read(entry);
    if (this.disposed) throw new Error('The symlink diff is no longer available.');
    if (this.documents.size >= 256) throw new Error('Too many symlink diffs are open.');
    entry.uri = fileUri.with({ scheme: 'gitpane-link', path: `/${randomUUID()}`, query: '', fragment: '' });
    const key = entry.uri.toString();
    entry.timer = setTimeout(() => this.remove(key), 60000);
    entry.watchToken = this.watchTokenFor(repo, relative) ?? ++this.nextWatchToken;
    this.documents.set(key, entry);
    revision?.activate?.(entry.watchToken);
    entry.fingerprint = this.fingerprint(entry, initialContent);
    return entry.uri;
  }

  async createStatic(fileUri, content, guard, guardSync, group) {
    if (this.disposed || this.documents.size >= 256) throw new Error('Too many Gitpane diffs are open.');
    if (typeof content !== 'string' || !/^Subproject commit [a-f0-9]{40,64}\n$/.test(content)) {
      throw new Error('Invalid submodule pointer contents.');
    }
    const entry = { content, guard, guardSync, group, closed: false };
    await this.read(entry);
    if (this.disposed || this.documents.size >= 256) throw new Error('The Gitpane diff is no longer available.');
    entry.uri = fileUri.with({ scheme: 'gitpane-link', path: `/${randomUUID()}`, query: '', fragment: '' });
    const key = entry.uri.toString();
    entry.timer = setTimeout(() => this.remove(key), 60000);
    entry.fingerprint = 'static';
    this.documents.set(key, entry);
    return entry.uri;
  }

  async validateStaticIdentity(repo, identity, guard) {
    if (!guard() || this.disposed) return false;
    const key = JSON.stringify([repo, identity]);
    let pending = this.identityChecks.get(key);
    if (!pending) {
      pending = (async () => {
        await this.acquire();
        try { return await this.backend.validateRepositoryIdentity(repo, identity); }
        finally { this.release(); }
      })();
      this.identityChecks.set(key, pending);
      pending.finally(() => { if (this.identityChecks.get(key) === pending) this.identityChecks.delete(key); }).catch(() => {});
    }
    const result = await pending;
    if (!result || typeof result.valid !== 'boolean') throw new Error('Gitpane returned an invalid repository identity result.');
    return !this.disposed && guard() && result.valid;
  }

  async provideTextDocumentContent(uri) {
    const entry = this.documents.get(uri.toString());
    if (!entry) throw new Error('The symlink diff is no longer available.');
    const content = await this.read(entry);
    if (this.documents.get(uri.toString()) !== entry) throw new Error('The symlink diff is no longer available.');
    return content;
  }

  remove(key) {
    const entry = this.documents.get(key);
    if (!entry) return;
    entry.closed = true;
    clearTimeout(entry.timer);
    this.documents.delete(key);
  }

  watchedPaths(repo) {
    const target = this.repoKey(repo);
    return [...new Set([...this.documents.values()]
      .filter(entry => entry.repo && this.repoKey(entry.repo) === target).map(entry => entry.path))];
  }

  watchTokenFor(repo, relative) {
    const target = this.repoKey(repo);
    return [...this.documents.values()].find(entry => entry.repo && this.repoKey(entry.repo) === target && entry.path === relative)?.watchToken;
  }

  repoKey(value) {
    const resolved = path.resolve(value);
    return process.platform === 'win32' ? resolved.toLowerCase() : resolved;
  }

  dispose() {
    this.disposed = true;
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer);
    for (const waiter of this.waiters.splice(0)) waiter.reject(new Error('The symlink diff is no longer available.'));
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const key of this.documents.keys()) this.remove(key);
    this.emitter.dispose();
  }
}

module.exports = { LinkDocuments, statusRevision };
