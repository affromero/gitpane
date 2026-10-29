'use strict';

const path = require('node:path');
const fs = require('node:fs');

const KINDS = {
  index: new Set(['index_modified', 'index_added', 'index_deleted', 'index_renamed', 'index_copied', 'index_type_changed']),
  working: new Set(['working_modified', 'working_deleted', 'working_renamed', 'intent_to_add', 'type_changed']),
  untracked: new Set(['untracked', 'untracked_directory']), merge: new Set(['conflicted']),
  attention: new Set(['submodule_attention'])
};

function relativeFile(value) {
  if (typeof value !== 'string' || !value || value.includes('\0') || path.isAbsolute(value)) return false;
  if (process.platform === 'win32' && (value.includes('\\') || path.win32.isAbsolute(value) || /^[a-z]:/i.test(value))) return false;
  return !value.split('/').some(part => !part || part === '.' || part === '..');
}

function scopedFile(root, relative) {
  if (!relativeFile(relative)) throw new Error('Gitpane returned an invalid file path.');
  const parts = relative.split('/');
  let current = root;
  for (const part of parts.slice(0, -1)) {
    current = path.join(current, part);
    try {
      const stat = fs.lstatSync(current);
      if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error('File has an unsafe ancestor.');
    } catch (error) {
      if (error.code === 'ENOENT') break;
      throw error;
    }
  }
  return path.join(root, ...parts);
}

function validRecord(record) {
  if (!record || !Object.hasOwn(KINDS, record.group) || !KINDS[record.group].has(record.kind) ||
      !relativeFile(record.path) || typeof record.is_submodule !== 'boolean' ||
      !(record.original_path === null || relativeFile(record.original_path))) return false;
  if (['index_renamed', 'index_copied', 'working_renamed'].includes(record.kind) && !record.original_path) return false;
  if (record.group === 'attention' && (!record.is_submodule || record.original_path !== null)) return false;
  if (record.group !== 'merge') return record.conflict_stages === null;
  const stages = record.conflict_stages;
  return !!stages && ['base', 'ours', 'theirs'].every(key => stages[key] === null || relativeFile(stages[key])) &&
    ['base', 'ours', 'theirs'].some(key => stages[key] !== null);
}

function validatePage(page, expectedSnapshot, expectedCursor) {
  const cursor = value => value === null || (Number.isSafeInteger(value) && value >= 0);
  if (!page || typeof page.snapshot !== 'string' || !page.snapshot || page.snapshot.length > 256 ||
      (expectedSnapshot && page.snapshot !== expectedSnapshot) ||
      typeof page.revision !== 'string' || !/^[a-f0-9]{64}$/.test(page.revision) ||
      !Number.isSafeInteger(page.cursor) || page.cursor < 0 || page.cursor !== expectedCursor ||
      !cursor(page.previous_cursor) || !cursor(page.next_cursor) ||
      (page.cursor === 0 && page.previous_cursor !== null) ||
      (page.cursor > 0 && page.previous_cursor === null) ||
      (page.previous_cursor !== null && page.previous_cursor >= page.cursor) ||
      (page.next_cursor !== null && page.next_cursor <= page.cursor) ||
      !Array.isArray(page.changes) || page.changes.length > 128 ||
      (page.next_cursor !== null && (!page.changes.length || page.next_cursor !== page.cursor + page.changes.length)) ||
      !page.changes.every(validRecord)) throw new Error('Gitpane returned an invalid changes page.');
  const keys = new Set();
  for (const record of page.changes) {
    const key = JSON.stringify([record.group, record.path]);
    if (keys.has(key)) throw new Error('Gitpane returned duplicate changes.');
    keys.add(key);
  }
  return page;
}

class ChangePager {
  constructor(backend, changed, onError = () => {}) {
    this.backend = backend;
    this.changed = changed;
    this.onError = onError;
    this.entries = new Map();
    this.disposed = false;
  }

  get(id) { return this.entries.get(id); }

  async refresh(id, repo, guard) {
    if (this.disposed || !guard()) return;
    let state = this.entries.get(id);
    if (state?.pending) return state.pending;
    if (!state || state.repo !== repo) {
      state = { repo, guard, phase: 'idle', page: 0, cursor: 0, changes: [], revision: undefined };
      this.entries.set(id, state);
    }
    state.guard = guard;
    const previous = { revision: state.revision, cursor: state.cursor, page: state.page };
    state.phase = 'loading';
    state.error = undefined;
    this.changed();
    const current = () => !this.disposed && this.entries.get(id) === state && state.guard();
    const pending = Promise.resolve().then(async () => {
      try {
        let page = validatePage(await this.backend.beginChanges(repo), undefined, 0);
        if (!current()) { if (this.entries.size) this.releaseSnapshot(repo, page.snapshot); return; }
        let pageNumber = 0;
        if (previous.cursor && previous.revision === page.revision) {
          const retained = validatePage(await this.backend.nextChanges(repo, page.snapshot, previous.cursor), page.snapshot, previous.cursor);
          if (!current()) return;
          if (retained.revision !== page.revision) throw new Error('Gitpane returned inconsistent changes pages.');
          page = retained;
          pageNumber = previous.page;
        }
        Object.assign(state, page, { page: pageNumber, phase: 'ready', error: undefined });
      } catch (error) {
        if (!current()) return;
        state.phase = 'error';
        state.error = error.code === 'invalid_request' && error.message === 'unknown bridge method' ?
          'The configured Gitpane backend does not support change paging. Update the backend and retry.' :
          error.code === 'snapshot_stale' ? 'Changes expired. Retry to refresh.' : error.message || String(error);
        state.changes = [];
        state.snapshot = undefined;
        state.revision = undefined;
      } finally {
        if (this.entries.get(id) === state) { state.pending = undefined; this.changed(); }
      }
    });
    state.pending = pending;
    return pending;
  }

  async move(id, direction) {
    const state = this.entries.get(id);
    if (!state || state.phase !== 'ready' || state.pending || !state.guard()) return;
    const cursor = direction === 'next' ? state.next_cursor : direction === 'previous' ? state.previous_cursor : null;
    if (cursor === null || cursor === undefined) return;
    const snapshot = state.snapshot;
    const revision = state.revision;
    const nextPage = state.page + (direction === 'next' ? 1 : -1);
    state.phase = 'loading';
    this.changed();
    const current = () => !this.disposed && this.entries.get(id) === state && state.guard();
    const pending = Promise.resolve().then(async () => {
      try {
        const page = validatePage(await this.backend.nextChanges(state.repo, snapshot, cursor), snapshot, cursor);
        if (!current()) { if (this.entries.size) this.releaseSnapshot(state.repo, snapshot); return; }
        if (page.revision !== revision) throw new Error('Gitpane returned inconsistent changes pages.');
        Object.assign(state, page, { page: nextPage, phase: 'ready', error: undefined });
      } catch (error) {
        if (!current()) return;
        state.phase = 'error';
        state.error = error.code === 'snapshot_stale' ? 'Changes expired. Retry to refresh.' : error.message || String(error);
        state.changes = [];
      } finally {
        if (this.entries.get(id) === state) { state.pending = undefined; this.changed(); }
      }
    });
    state.pending = pending;
    return pending;
  }

  release(id) {
    const state = this.entries.get(id);
    this.entries.delete(id);
    if (!this.entries.size && typeof this.backend.cancelChanges === 'function') {
      this.backend.cancelChanges();
      return;
    }
    if (state?.snapshot && !state.pending && state.guard()) this.releaseSnapshot(state.repo, state.snapshot);
  }

  releaseSnapshot(repo, snapshot) {
    Promise.resolve().then(() => this.backend.releaseChanges(repo, snapshot)).catch(error => {
      if (!['snapshot_stale', 'workspace_changed', 'outside_workspace', 'disposed'].includes(error?.code)) this.onError(error);
    });
  }

  dispose() {
    this.disposed = true;
    for (const id of this.entries.keys()) this.release(id);
  }
}

module.exports = { ChangePager, relativeFile, scopedFile, validRecord, validatePage };
