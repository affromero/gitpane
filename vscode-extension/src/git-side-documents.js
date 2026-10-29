'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

const oid = value => typeof value === 'string' && /^[a-f0-9]{40}$/.test(value);
const identity = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

function validMetadata(value, ref) {
  return value && oid(value.oid) && Number.isSafeInteger(value.size) && value.size >= 0 &&
    [0o100644, 0o100664, 0o100755, 0o120000].includes(value.mode) &&
    (ref === 'HEAD' ? oid(value.head_oid) : value.head_oid === null);
}

// A git: URI can be routed to another open repository based on its path.
// For a Git side hidden behind a blocked worktree ancestor, bind reads to the
// selected repository instead. The path is a label and is never opened on disk.
async function createGitSide(options) {
  const { vscode, links, backend, repo, repoIdentity, repoToken, side, record, current, verify, verifyAncestors, ancestorKey, contextKey } = options;
  if (!links || typeof backend.repoBlobMetadata !== 'function' || typeof repo?.buffer !== 'function' ||
      !identity(repoIdentity) || !relativeFile(side?.gitPath) ||
      !['HEAD', '', ':1', ':2', ':3'].includes(side.ref) ||
      (verifyAncestors && (typeof ancestorKey !== 'string' || !ancestorKey))) {
    throw new Error('Gitpane cannot open this Git side. Update the backend and try again.');
  }
  const repoPath = repo.rootUri.fsPath;
  const metadata = async () => {
    const result = await backend.repoBlobMetadata(repoPath, repoIdentity, side.ref, side.gitPath);
    if (!validMetadata(result, side.ref)) throw new Error('Gitpane returned invalid Git file metadata.');
    return result;
  };
  if (!current()) throw new Error('The selected change is no longer available.');
  const expected = await metadata();
  const signature = JSON.stringify([expected.oid, expected.mode, expected.size, expected.head_oid]);
  const sameBlob = value => JSON.stringify([value.oid, value.mode, value.size, value.head_oid]) === signature;
  const validatedMetadata = async () => {
    if (!current() || (verifyAncestors && !await verifyAncestors()) || !current()) return false;
    if (!await verify() || !current()) return false;
    const latest = await metadata();
    if (!sameBlob(latest) || !current()) return false;
    if (verifyAncestors && (!await verifyAncestors() || !current())) return false;
    return latest;
  };
  const readMetadata = async () => {
    const latest = await validatedMetadata();
    if (!latest) throw new Error('The Git side changed. Refresh and reopen the diff.');
    return { size: latest.size };
  };
  const uri = vscode.Uri.file(path.join(repoPath, ...side.gitPath.split('/')));
  const group = JSON.stringify([repoPath, repoIdentity, repoToken, record, side.ref, side.gitPath, signature, ancestorKey, contextKey]);
  const readRef = side.ref === 'HEAD' ? expected.head_oid : side.ref;
  return links.createBinary(uri, readMetadata, () => repo.buffer(readRef, side.gitPath), current, current, group,
    async () => !!await validatedMetadata());
}

module.exports = { createGitSide };
