'use strict';

const { BridgeError, MAX_REQUEST_BYTES } = require('./bridge-client');
const { relativeFile } = require('./change-pager');

class GitMetadataBatches {
  constructor(dispatch) {
    this.dispatch = dispatch;
    this.groups = new Map();
    this.ready = [];
    this.active = new Set();
    this.scheduled = undefined;
    this.disposed = false;
  }

  request(session, identity, item, signal) {
    if (this.disposed) return Promise.reject(new BridgeError('disposed', 'Gitpane metadata service was closed.'));
    if (signal?.aborted) return Promise.reject(new BridgeError('cancelled', 'Gitpane metadata request was cancelled.'));
    if (!item || typeof item !== 'object' || Array.isArray(item) || Object.keys(item).sort().join(',') !== 'path,ref' ||
        !relativeFile(item.path) || !(['HEAD', ':0', ':1', ':2', ':3'].includes(item.ref) ||
          (typeof item.ref === 'string' && /^[a-f0-9]{40}$/.test(item.ref)))) {
      return Promise.reject(new BridgeError('invalid_request', 'Invalid Git content reference or path.'));
    }
    const { snapshot, git, repo } = session;
    const key = JSON.stringify([snapshot.executable, snapshot.generation, snapshot.roots, snapshot.rootSources,
      snapshot.rootIdentities, repo, identity, git.executable, git.key]);
    return new Promise((resolve, reject) => {
      const entry = { item, signal, resolve, reject, settled: false };
      entry.abort = () => {
        this.settle(entry, new BridgeError('cancelled', 'Gitpane metadata request was cancelled.'));
        if (entry.batch && entry.batch.entries.every(candidate => candidate.settled)) entry.batch.controller.abort();
      };
      signal?.addEventListener('abort', entry.abort, { once: true });
      if (signal?.aborted) { entry.abort(); return; }
      let group = this.groups.get(key);
      if (!group) { group = { session, identity, entries: [] }; this.groups.set(key, group); }
      group.entries.push(entry);
      if (this.scheduled === undefined) this.scheduled = setImmediate(() => this.flush());
    });
  }

  settle(entry, error, result) {
    if (entry.settled) return;
    entry.settled = true;
    entry.signal?.removeEventListener('abort', entry.abort);
    if (error) entry.reject(error);
    else entry.resolve(result);
  }

  flush() {
    this.scheduled = undefined;
    const groups = [...this.groups.values()];
    this.groups.clear();
    for (const group of groups) {
      const { snapshot, git, repo } = group.session;
      const base = Buffer.byteLength(`${JSON.stringify({ version: 1, workspace_roots: snapshot.roots,
        workspace_root_identities: snapshot.rootIdentities, workspace_root_sources: snapshot.rootSources,
        repo, expected_repo_identity: group.identity, git_executable: git.executable,
        operation: { kind: 'metadata', items: [] } })}\n`);
      let entries = [], bytes = base;
      const queue = () => {
        if (entries.length) this.ready.push({ ...group, entries, controller: new AbortController() });
        entries = [];
        bytes = base;
      };
      for (const entry of group.entries) {
        if (entry.settled) continue;
        const size = Buffer.byteLength(JSON.stringify(entry.item));
        if (base + size > MAX_REQUEST_BYTES) {
          this.settle(entry, new BridgeError('request_too_large', 'Git content request is too large.'));
          continue;
        }
        if (entries.length === 128 || bytes + size + (entries.length ? 1 : 0) > MAX_REQUEST_BYTES) queue();
        bytes += size + (entries.length ? 1 : 0);
        entries.push(entry);
      }
      queue();
    }
    this.pump();
  }

  pump() {
    while (!this.disposed && this.active.size < 4 && this.ready.length) {
      const batch = this.ready.shift();
      batch.entries = batch.entries.filter(entry => !entry.settled);
      if (!batch.entries.length) continue;
      for (const entry of batch.entries) entry.batch = batch;
      this.active.add(batch);
      Promise.resolve().then(() => this.dispatch(batch.session, batch.identity, batch.entries.map(entry => entry.item), batch.controller.signal))
        .then(response => {
          if (!response || response.version !== 1 || typeof response.volatile !== 'boolean' ||
              typeof response.replacements !== 'string' || typeof response.config !== 'string' ||
              !/^[a-f0-9]{64}$/.test(response.replacements) || !/^[a-f0-9]{64}$/.test(response.config) ||
              !Array.isArray(response.results) || response.results.length !== batch.entries.length) {
            throw new BridgeError('protocol_error', 'Gitpane returned invalid Git file metadata checks.');
          }
          for (let index = 0; index < batch.entries.length; index++) {
            this.settle(batch.entries[index], undefined, { ...response, results: [response.results[index]] });
          }
        })
        .catch(error => { for (const entry of batch.entries) this.settle(entry, error); })
        .finally(() => { this.active.delete(batch); this.pump(); });
    }
  }

  cancel() {
    if (this.scheduled !== undefined) clearImmediate(this.scheduled);
    this.scheduled = undefined;
    const error = new BridgeError('cancelled', 'Gitpane metadata requests were cancelled.');
    for (const group of this.groups.values()) for (const entry of group.entries) this.settle(entry, error);
    this.groups.clear();
    for (const batch of this.ready.splice(0)) for (const entry of batch.entries) this.settle(entry, error);
    for (const batch of this.active) {
      for (const entry of batch.entries) this.settle(entry, error);
      batch.controller.abort();
    }
  }

  dispose() { this.disposed = true; this.cancel(); }
}

module.exports = { GitMetadataBatches };
