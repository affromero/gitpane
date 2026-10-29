'use strict';

const { randomUUID } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

// A Git symlink stores the link text, not the contents of the linked file.
// Keep paths and guards in the extension host, never in the document URI.
class LinkDocuments {
  constructor(vscode, tree, backend) {
    this.vscode = vscode;
    this.backend = backend;
    this.output = tree.output;
    this.documents = new Map();
    this.disposed = false;
    this.activeReads = 0;
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
    return !this.disposed && !entry.closed && !!current;
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

  fingerprint(entry) {
    let identity;
    try {
      const stat = fs.lstatSync(path.join(entry.repo, entry.path), { bigint: true });
      identity = [stat.dev, stat.ino, stat.mode, stat.size, stat.mtimeNs, stat.ctimeNs].join(':');
    } catch (error) { identity = `error:${error.code || error.message}`; }
    let revision;
    try { revision = entry.revision?.(); }
    catch (error) { revision = `error:${error.message}`; }
    return JSON.stringify([identity, revision ?? null]);
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
      for (const entry of this.documents.values()) {
        let current;
        try { current = await this.current(entry); } catch { current = false; }
        if (this.disposed || entry.closed) continue;
        const next = current ? this.fingerprint(entry) : 'stale';
        if (entry.fingerprint !== next) {
          entry.fingerprint = next;
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
    await this.read(entry);
    if (this.disposed) throw new Error('The symlink diff is no longer available.');
    if (this.documents.size >= 256) throw new Error('Too many symlink diffs are open.');
    entry.uri = fileUri.with({ scheme: 'gitpane-link', path: `/${randomUUID()}`, query: '', fragment: '' });
    const key = entry.uri.toString();
    entry.fingerprint = this.fingerprint(entry);
    entry.timer = setTimeout(() => this.remove(key), 60000);
    this.documents.set(key, entry);
    return entry.uri;
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

  dispose() {
    this.disposed = true;
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer);
    for (const waiter of this.waiters.splice(0)) waiter.reject(new Error('The symlink diff is no longer available.'));
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const key of this.documents.keys()) this.remove(key);
    this.emitter.dispose();
  }
}

module.exports = { LinkDocuments };
