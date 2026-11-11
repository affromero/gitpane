'use strict';

const { relativeFile } = require('./change-pager');

const OID = /^[a-f0-9]{40,64}$/;
const IDENTITY = /^[a-f0-9]{64}$/;
const SOURCES = new Map([['tracked', 'tracked'], ['saved_untracked', 'saved-untracked']]);
const STATUSES = new Set(['A', 'D', 'M', 'R', 'T']);
const KINDS = new Set(['missing', 'text', 'binary', 'gitlink']);

const invalidList = () => new Error('Gitpane returned an invalid stash file list.');
const invalidContent = () => new Error('Gitpane returned invalid stash file contents.');
const optionalOid = value => value === null || value === undefined || OID.test(value || '');
const optionalPath = value => value === null || value === undefined || relativeFile(value);

// The bridge reports the saved tree position as `saved_untracked`; the
// extension model uses `saved-untracked` for display and test readability.
function normalizeFile(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw invalidList();
  const source = SOURCES.get(raw.source);
  if (!source || typeof raw.status !== 'string' || !STATUSES.has(raw.status) ||
      !relativeFile(raw.path) || !optionalPath(raw.old_path) || !optionalPath(raw.new_path) ||
      typeof raw.is_binary !== 'boolean' || typeof raw.is_submodule !== 'boolean') throw invalidList();
  const oldPath = raw.old_path ?? null, newPath = raw.new_path ?? null;
  if (raw.status === 'R' && (oldPath === null || oldPath === raw.path)) throw invalidList();
  return { source, status: raw.status, path: raw.path, old_path: oldPath, new_path: newPath,
    binary: raw.is_binary, gitlink: raw.is_submodule };
}

function normalizeList(response, oid, identity) {
  if (!response || typeof response !== 'object' || Array.isArray(response) ||
      response.oid !== oid || response.repo_identity !== identity ||
      typeof response.message !== 'string' || typeof response.parent_oid !== 'string' || !OID.test(response.parent_oid) ||
      !optionalOid(response.untracked_parent_oid) || !Array.isArray(response.files) ||
      response.files.length > 10000) throw invalidList();
  return {
    stash_oid: response.oid, repo_identity: response.repo_identity, message: response.message,
    parent_oid: response.parent_oid, untracked_parent_oid: response.untracked_parent_oid ?? null,
    files: response.files.map(normalizeFile)
  };
}

function normalizeSide(raw) {
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== 'object' || Array.isArray(raw) || !KINDS.has(raw.kind) ||
      (raw.text !== null && raw.text !== undefined && typeof raw.text !== 'string')) throw invalidContent();
  if (raw.kind === 'binary') return { kind: 'binary', text: null };
  if (typeof raw.text !== 'string') throw invalidContent();
  return { kind: raw.kind, text: raw.text };
}

function normalizeContent(response, oid, identity, source, file) {
  if (!response || typeof response !== 'object' || Array.isArray(response) ||
      response.oid !== oid || response.repo_identity !== identity ||
      !response.file || typeof response.file !== 'object' || Array.isArray(response.file) ||
      response.file.source !== source || response.file.path !== file.path) throw invalidContent();
  return { old: normalizeSide(response.old), new: normalizeSide(response.new) };
}

// The shared bridge API for stash snapshots. Requests carry the immutable stash
// OID and the expected repository identity, never a stash index, so a dropped or
// renumbered stash cannot silently resolve to a different saved object.
function createStashSource(backend) {
  if (!backend || typeof backend.stashFiles !== 'function' || typeof backend.stashFileContent !== 'function') {
    return unsupportedStashSource();
  }
  return {
    async listFiles({ repo, identity, oid }) {
      return normalizeList(await backend.stashFiles(repo, oid), oid, identity);
    },
    async readFile({ repo, identity, oid, file }) {
      const source = file.source === 'saved-untracked' ? 'saved_untracked' : 'tracked';
      const response = await backend.stashFileContent(repo, oid, identity, source, file.path);
      return normalizeContent(response, oid, identity, source, file);
    }
  };
}

function unsupportedStashSource() {
  const fail = () => {
    const error = new Error('The configured Gitpane backend does not support stash previews. Update the backend and try again.');
    error.code = 'unsupported_backend';
    return Promise.reject(error);
  };
  return { listFiles: fail, readFile: fail };
}

module.exports = { createStashSource, normalizeContent, normalizeList, unsupportedStashSource };
