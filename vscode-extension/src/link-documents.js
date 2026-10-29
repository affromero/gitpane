'use strict';

const { randomUUID } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { MAX_REQUEST_BYTES } = require('./bridge-client');

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
    this.activeBinaryReads = 0;
    this.binaryWaiters = [];
    this.identityChecks = new Map();
    this.waiters = [];
    this.refreshTimer = undefined;
    this.refreshing = false;
    this.refreshAgain = false;
    this.emitter = new vscode.EventEmitter();
    this.onDidChange = this.emitter.event;
    this.fileEmitter = new vscode.EventEmitter();
    this.onDidChangeFile = this.fileEmitter.event;
    this.subscriptions = [
      tree.onDidChangeTreeData(() => this.scheduleRefresh()),
      vscode.workspace.onDidOpenTextDocument?.(document => {
        const entry = this.documents.get(document.uri.toString());
        if (!entry) return;
        if (entry.readBinary && vscode.window?.tabGroups) entry.textOpen = true;
        clearTimeout(entry.timer);
        entry.timer = undefined;
      }) || { dispose() {} },
      vscode.workspace.onDidCloseTextDocument?.(document => {
        const key = document.uri.toString(), entry = this.documents.get(key);
        if (!entry?.readBinary || !vscode.window?.tabGroups) { this.remove(key); return; }
        const wasOpen = entry.textOpen;
        entry.textOpen = false;
        if (entry.tabOpen || !wasOpen) return;
        if (entry.opened) this.expirePendingBinary(key, entry, 100);
        else this.expirePendingBinary(key, entry);
      }) || { dispose() {} },
      vscode.window?.tabGroups?.onDidChangeTabs?.(() => this.updateBinaryTabs()) || { dispose() {} }
    ];
  }

  async read(entry) {
    if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
    if (entry.content !== undefined) return entry.content;
    await this.acquire();
    try {
      if (!await this.current(entry)) throw new Error('The symlink change is no longer in the workspace.');
      if (entry.readContent) {
        const content = await entry.readContent();
        if (typeof content !== 'string' || !await this.current(entry)) throw new Error('The Gitpane diff is no longer available.');
        return content;
      }
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
    if (entry.content !== undefined || entry.readContent || entry.readBinary) return 'static';
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

  async pointerBatches(entries) {
    const repositories = new Map(), results = new Map();
    for (const entry of entries) {
      if (!entry.batch || entry.closed || (entry.guardSync && !entry.guardSync())) continue;
      const key = JSON.stringify([entry.batch.repo, entry.batch.identity]);
      if (!repositories.has(key)) repositories.set(key, { repo: entry.batch.repo, identity: entry.batch.identity, groups: new Map() });
      repositories.get(key).groups.set(entry.group, entry.batch);
    }
    const checkAncestors = async (batch, checks) => {
      if (!batch.verifyAncestors) return true;
      if (typeof batch.ancestorKey !== 'string' || !batch.ancestorKey) throw new Error('Invalid submodule ancestor guard.');
      if (!checks.has(batch.ancestorKey)) checks.set(batch.ancestorKey, Promise.resolve().then(() => batch.verifyAncestors()));
      return await checks.get(batch.ancestorKey) === true;
    };
    for (const item of repositories.values()) {
      const groups = [...item.groups];
      const baseBytes = Buffer.byteLength(`${JSON.stringify({ id: Number.MAX_SAFE_INTEGER, method: 'submodule_pointer_validate_batch',
        params: { repo: item.repo, expected_repo_identity: item.identity, snapshots: [] } })}\n`);
      let start = 0;
      while (start < groups.length && !this.disposed) {
        const batch = [];
        let bytes = baseBytes;
        while (start < groups.length && batch.length < 128) {
          const candidate = groups[start];
          const size = Buffer.byteLength(JSON.stringify(candidate[1].snapshot)) + (batch.length ? 1 : 0);
          if (bytes + size > MAX_REQUEST_BYTES) {
            if (batch.length) break;
            results.set(candidate[0], { valid: false, code: 'request_too_large',
              message: 'This submodule pointer snapshot exceeds the supported request size.' });
            start++;
            continue;
          }
          batch.push(candidate);
          bytes += size;
          start++;
        }
        if (!batch.length) continue;
        const before = new Map(), eligible = [];
        for (const [group, entry] of batch) {
          try {
            if (await checkAncestors(entry, before)) eligible.push([group, entry]);
            else results.set(group, { valid: false, code: 'stale_target', message: 'The parent submodule changed.' });
          } catch (error) { results.set(group, { valid: false, code: error?.code || 'error', message: error?.message || String(error) }); }
        }
        if (!eligible.length || this.disposed) continue;
        try {
          const response = await this.backend.validatePointerBatch(item.repo, item.identity, eligible.map(([, entry]) => entry.snapshot));
          if (!response || !Array.isArray(response.results) || response.results.length !== eligible.length ||
              response.results.some(value => !value || typeof value.valid !== 'boolean' ||
                (!value.valid && (typeof value.code !== 'string' || typeof value.message !== 'string')))) {
            throw new Error('Gitpane returned invalid submodule pointer checks.');
          }
          const after = new Map();
          for (let index = 0; index < eligible.length; index++) {
            const [group, entry] = eligible[index];
            if (!response.results[index].valid) { results.set(group, response.results[index]); continue; }
            try {
              results.set(group, await checkAncestors(entry, after) ? response.results[index] :
                { valid: false, code: 'stale_target', message: 'The parent submodule changed.' });
            } catch (error) { results.set(group, { valid: false, code: error?.code || 'error', message: error?.message || String(error) }); }
          }
        } catch (error) {
          for (const [group] of eligible) results.set(group, { valid: false, code: error?.code || 'error', message: error?.message || String(error) });
        }
      }
    }
    return results;
  }

  async refresh() {
    if (this.refreshing) { this.refreshAgain = true; return; }
    this.refreshing = true;
    try {
      const checks = new Map(), probes = new Map(), shown = new Set();
      const entries = [...this.documents.values()];
      const pointerResults = await this.pointerBatches(entries);
      for (const entry of entries) {
        let current, failure;
        try {
          if (entry.batch) {
            const result = pointerResults.get(entry.group);
            current = !!result?.valid && (!entry.guardSync || entry.guardSync());
            if (result && !result.valid && result.code !== 'stale_target') {
              failure = Object.assign(new Error(result.message), { code: result.code });
            }
          } else if ((entry.content !== undefined || entry.readContent || entry.readBinary) && entry.group) {
            if (!checks.has(entry.group)) checks.set(entry.group, entry.refreshGuard ? entry.refreshGuard() : this.current(entry));
            current = await checks.get(entry.group) && (!entry.guardSync || entry.guardSync());
          } else current = await this.current(entry);
        }
        catch (error) { current = false; failure = error; }
        let target;
        if (current && entry.content === undefined && !entry.readContent && !entry.readBinary) {
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
          if (entry.batch && failure) {
            const key = JSON.stringify([entry.batch.repo, failure.code, failure.message]);
            if (!shown.has(key)) { shown.add(key); this.vscode.window?.showErrorMessage(`Gitpane could not validate a submodule diff: ${failure.message}`); }
          }
          if (entry.readBinary) this.fileEmitter.fire([{ type: this.vscode.FileChangeType.Changed, uri: entry.uri }]);
          else this.emitter.fire(entry.uri);
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

  async createVerified(fileUri, readContent, guard, guardSync, group, batch) {
    if (this.disposed || this.documents.size >= 256) throw new Error('Too many Gitpane diffs are open.');
    const entry = { readContent, guard, guardSync, group, batch, closed: false };
    await this.read(entry);
    if (this.disposed || this.documents.size >= 256) throw new Error('The Gitpane diff is no longer available.');
    entry.uri = fileUri.with({ scheme: 'gitpane-link', path: `/${randomUUID()}`, query: '', fragment: '' });
    const key = entry.uri.toString();
    entry.timer = setTimeout(() => this.remove(key), 60000);
    entry.fingerprint = 'static';
    this.documents.set(key, entry);
    return entry.uri;
  }

  async binaryMetadata(entry) {
    if (!await this.current(entry)) throw new Error('The Gitpane diff is no longer available.');
    const metadata = await entry.readMetadata();
    if (!metadata || !Number.isSafeInteger(metadata.size) || metadata.size < 0) {
      throw new Error('Gitpane returned invalid historical file metadata.');
    }
    if (!await this.current(entry)) throw new Error('The Gitpane diff is no longer available.');
    return metadata;
  }

  async createBinary(fileUri, readMetadata, readBinary, guard, guardSync, group, refreshGuard) {
    if (this.disposed || this.documents.size >= 256) throw new Error('Too many Gitpane diffs are open.');
    const entry = { readMetadata, readBinary, guard, guardSync, group, refreshGuard, closed: false, textOpen: false, tabOpen: false };
    await this.binaryMetadata(entry);
    if (this.disposed || this.documents.size >= 256) throw new Error('The Gitpane diff is no longer available.');
    const name = path.basename(fileUri.fsPath);
    entry.uri = fileUri.with({ scheme: 'gitpane-blob', path: `/${randomUUID()}/${name}`, query: '', fragment: '' });
    const key = entry.uri.toString();
    entry.fingerprint = 'static';
    this.documents.set(key, entry);
    this.expirePendingBinary(key, entry);
    return entry.uri;
  }

  expirePendingBinary(key, entry, delay = 60000) {
    clearTimeout(entry.timer);
    entry.timer = setTimeout(() => {
      entry.timer = undefined;
      if (this.documents.get(key) === entry && !entry.textOpen && !entry.tabOpen) this.remove(key);
    }, delay);
  }

  binaryEntry(uri) {
    const entry = this.documents.get(uri.toString());
    if (!entry?.readBinary || entry.closed) throw new Error('The Gitpane diff is no longer available.');
    return entry;
  }

  async stat(uri) {
    const entry = this.binaryEntry(uri);
    const metadata = await this.binaryMetadata(entry);
    if (this.binaryEntry(uri) !== entry) throw new Error('The Gitpane diff is no longer available.');
    return { type: this.vscode.FileType.File, size: metadata.size, ctime: 0, mtime: 0 };
  }

  async readFile(uri) {
    const entry = this.binaryEntry(uri);
    if (entry.refreshGuard) {
      if (!await this.current(entry)) throw new Error('The Gitpane diff is no longer available.');
    } else await this.binaryMetadata(entry);
    await this.acquireBinary();
    try {
      if (this.binaryEntry(uri) !== entry) throw new Error('The Gitpane diff is no longer available.');
      await this.binaryMetadata(entry);
      const bytes = await entry.readBinary();
      if (!(bytes instanceof Uint8Array)) throw new Error('VS Code returned invalid historical file contents.');
      await this.binaryMetadata(entry);
      if (this.binaryEntry(uri) !== entry) throw new Error('The Gitpane diff is no longer available.');
      return bytes;
    } finally { this.releaseBinary(); }
  }

  acquireBinary() {
    if (this.disposed) return Promise.reject(new Error('The Gitpane diff is no longer available.'));
    if (this.activeBinaryReads < 2) { this.activeBinaryReads++; return Promise.resolve(); }
    return new Promise((resolve, reject) => this.binaryWaiters.push({ resolve, reject }));
  }

  releaseBinary() {
    const next = this.binaryWaiters.shift();
    if (next) next.resolve();
    else this.activeBinaryReads--;
  }

  watch() { return { dispose() {} }; }
  readDirectory() { throw this.vscode.FileSystemError.NoPermissions(); }
  createDirectory() { throw this.vscode.FileSystemError.NoPermissions(); }
  writeFile() { throw this.vscode.FileSystemError.NoPermissions(); }
  delete() { throw this.vscode.FileSystemError.NoPermissions(); }
  rename() { throw this.vscode.FileSystemError.NoPermissions(); }

  updateBinaryTabs() {
    const open = new Set();
    for (const group of this.vscode.window.tabGroups.all) for (const tab of group.tabs) {
      for (const uri of [tab.input?.uri, tab.input?.original, tab.input?.modified]) {
        if (uri) open.add(uri.toString());
      }
    }
    for (const [key, entry] of this.documents) {
      if (!entry.readBinary) continue;
      const wasOpen = entry.tabOpen;
      entry.tabOpen = open.has(key);
      if (entry.tabOpen) { entry.opened = true; clearTimeout(entry.timer); entry.timer = undefined; }
      else if (wasOpen && !entry.textOpen) this.expirePendingBinary(key, entry, 100);
    }
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
    for (const waiter of this.binaryWaiters.splice(0)) waiter.reject(new Error('The Gitpane diff is no longer available.'));
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const key of this.documents.keys()) this.remove(key);
    this.emitter.dispose();
    this.fileEmitter.dispose();
  }
}

module.exports = { LinkDocuments, statusRevision };
