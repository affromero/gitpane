'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

const oid = value => typeof value === 'string' && /^[a-f0-9]{40}$/.test(value);
const identity = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

function validMetadata(value, ref) {
  return value && typeof value === 'object' && !Array.isArray(value) &&
    Object.keys(value).sort().join(',') === 'head_oid,mode,oid,size' &&
    oid(value.oid) && Number.isSafeInteger(value.size) && value.size >= 0 &&
    [0o100644, 0o100664, 0o100755, 0o120000].includes(value.mode) &&
    (ref === 'HEAD' ? oid(value.head_oid) : oid(ref) ? value.head_oid === ref : value.head_oid === null);
}

function metadataResult(response, ref, revision) {
  if (!response || typeof response !== 'object' || Array.isArray(response) || response.version !== 1 ||
      typeof response.volatile !== 'boolean' || !identity(response.replacements) || !identity(response.config) ||
      !Array.isArray(response.results) || response.results.length !== 1) {
    throw new Error('Gitpane returned invalid Git file metadata.');
  }
  const value = response.results[0];
  const keys = value && typeof value === 'object' && !Array.isArray(value) ? Object.keys(value) : [];
  if (keys.length === 1 && keys[0] === 'error' && value.error &&
      typeof value.error === 'object' && !Array.isArray(value.error) &&
      Object.keys(value.error).sort().join(',') === 'code,message' &&
      typeof value.error.code === 'string' && typeof value.error.message === 'string') {
    throw Object.assign(new Error(value.error.message), { code: value.error.code });
  }
  const result = keys.length === 1 && keys[0] === 'metadata' ? value.metadata : undefined;
  if (!validMetadata(result, ref)) throw new Error('Gitpane returned invalid Git file metadata.');
  const fingerprint = JSON.stringify([result.oid, result.mode, result.size, result.head_oid,
    response.replacements, response.config, response.volatile]);
  return { size: result.size, ref: ref === 'HEAD' ? result.head_oid : ref === '' ? ':0' : ref, fingerprint,
    eventFingerprint: JSON.stringify([fingerprint, response.volatile ? revision?.() ?? null : null]) };
}

async function createLiveGitSide(options) {
  const { vscode, links, backend, repo, repoIdentity, repoToken, side, current, verifyAncestors, ancestorKey, contextKey, revision } = options;
  if (!links || typeof links.createLiveBinary !== 'function' || typeof backend.gitSideSession !== 'function' ||
      typeof backend.gitSideOperation !== 'function' || !repo?.rootUri || !identity(repoIdentity) || !relativeFile(side?.gitPath) ||
      !(['HEAD', '', ':1', ':2', ':3'].includes(side.ref) || oid(side.ref)) ||
      (verifyAncestors && (typeof ancestorKey !== 'string' || !ancestorKey))) {
    throw new Error('Gitpane cannot open this Git side. Update the backend and try again.');
  }
  const repoPath = repo.rootUri.fsPath;
  const session = await backend.gitSideSession(repoPath);
  const guard = () => current() && backend.gitSideCurrent(session);
  const uri = vscode.Uri.file(path.join(repoPath, ...side.gitPath.split('/')));
  const metadata = async signal => {
    if (!guard() || (verifyAncestors && !await verifyAncestors()) || !guard()) {
      throw new Error('The Gitpane diff is no longer in the workspace.');
    }
    let result;
    const capturedRevision = revision?.() ?? null;
    try {
      const response = await backend.gitSideOperation(session, repoIdentity,
        { kind: 'metadata', items: [{ ref: side.ref || ':0', path: side.gitPath }] }, signal);
      result = metadataResult(response, side.ref, () => capturedRevision);
    }
    catch (error) {
      if (error?.code === 'content_not_found') throw vscode.FileSystemError.FileNotFound(uri);
      throw error;
    }
    if (!guard() || (verifyAncestors && !await verifyAncestors()) || !guard()) {
      throw new Error('The Gitpane diff is no longer in the workspace.');
    }
    return result;
  };
  const group = JSON.stringify([repoPath, repoIdentity, repoToken, side.ref, side.gitPath, ancestorKey, contextKey, session.git.key]);
  const batch = { repo: repoPath, identity: repoIdentity, session, item: { ref: side.ref || ':0', path: side.gitPath },
    verifyAncestors, ancestorKey, contextKey, revision, convert(value, response, capturedRevision) {
      try { return metadataResult({ ...response, results: [value] }, side.ref, () => capturedRevision).eventFingerprint; }
      catch (error) {
        if (error?.code === 'content_not_found') return 'missing';
        throw error;
      }
    } };
  const read = async (_state, signal) => {
    if (!guard() || (verifyAncestors && !await verifyAncestors()) || !guard()) {
      throw new Error('The Gitpane diff is no longer in the workspace.');
    }
    let bytes;
    try {
      bytes = await backend.gitSideOperation(session, repoIdentity,
        { kind: 'verified_content', ref: side.ref || ':0', path: side.gitPath }, signal);
    } catch (error) {
      if (error?.code === 'content_not_found') throw vscode.FileSystemError.FileNotFound(uri);
      throw error;
    }
    if (!guard() || (verifyAncestors && !await verifyAncestors()) || !guard()) {
      throw new Error('The Gitpane diff is no longer in the workspace.');
    }
    return bytes;
  };
  return links.createLiveBinary(uri, metadata, read, guard, group, batch);
}

module.exports = { createLiveGitSide };
