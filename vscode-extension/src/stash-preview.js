'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

const STASH_OID = /^[a-f0-9]{40,64}$/;
const REPO_IDENTITY = /^[a-f0-9]{64}$/;
const SOURCES = new Set(['tracked', 'saved-untracked']);
const STATUSES = new Set(['A', 'M', 'D', 'R', 'T']);
const KINDS = new Set(['missing', 'text', 'binary', 'gitlink']);

const isStashOid = value => typeof value === 'string' && STASH_OID.test(value);
const optionalPath = value => value === null || relativeFile(value);

// A stash saves two disjoint file sets. `tracked` is the first parent against
// the stash tree; `saved-untracked` is the empty tree against the third parent.
// Both are immutable objects, so the extension never reads the working tree.
function validStashFile(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      typeof value.source !== 'string' || !SOURCES.has(value.source) ||
      typeof value.status !== 'string' || !STATUSES.has(value.status) ||
      !relativeFile(value.path) || !optionalPath(value.old_path) || !optionalPath(value.new_path) ||
      typeof value.binary !== 'boolean' || typeof value.gitlink !== 'boolean') return false;
  return value.status === 'R' ? relativeFile(value.old_path) && value.old_path !== value.path : true;
}

function validateStashList(value, oid, identity) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      value.stash_oid !== oid || value.repo_identity !== identity ||
      !Array.isArray(value.files) || value.files.length > 10000 ||
      !value.files.every(validStashFile)) {
    throw new Error('Gitpane returned an invalid stash file list.');
  }
  const keys = new Set();
  for (const file of value.files) {
    const key = JSON.stringify([file.source, file.old_path, file.path]);
    if (keys.has(key)) throw new Error('Gitpane returned duplicate stash files.');
    keys.add(key);
  }
  return value;
}

function validStashSide(value) {
  if (value === null) return true;
  if (!value || typeof value !== 'object' || Array.isArray(value) || !KINDS.has(value.kind)) return false;
  if (value.kind === 'binary') return value.text === null;
  return typeof value.text === 'string';
}

function validateStashContent(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      !validStashSide(value.old) || !validStashSide(value.new)) {
    throw new Error('Gitpane returned invalid stash file contents.');
  }
  return value;
}

// Bridge errors stay actionable: a stale or missing saved object must never
// look like an empty diff, and a dropped stash must ask for a refresh.
function stashErrorMessage(error) {
  if (error?.code === 'stale_target') return 'The repository changed while reading the stash. Refresh and try again.';
  if (error?.code === 'content_not_found') return 'The saved stash or file is no longer available. Refresh and try again.';
  return error?.message || String(error);
}

function stashFileLabel(file) {
  const renamed = file.status === 'R' ? `${file.old_path} → ${file.path}` : file.path;
  return `${file.status} ${renamed}${file.binary ? ' (binary)' : ''}${file.gitlink ? ' (submodule)' : ''}`;
}

function stashSides(file) {
  const left = file.source === 'saved-untracked' || file.status === 'A' ? null :
    file.old_path ?? file.path;
  const right = file.status === 'D' ? null : file.new_path ?? file.path;
  return { left, right };
}

function stashUnavailableMessage(file) {
  if (file.binary) return `Gitpane cannot show a text diff for the saved binary file ${file.path}.`;
  return undefined;
}

function stashFilePath(repoPath, relative) {
  if (!relativeFile(relative)) throw new Error('Gitpane returned an invalid stash file path.');
  return path.join(repoPath, ...relative.split('/'));
}

const sideText = side => side && (side.kind === 'text' || side.kind === 'gitlink') ? side.text : '';

// Opens both sides from saved stash objects only. `contents` holds the two
// sides returned by the shared stash content request, so no working tree, index
// or HEAD content can leak into the preview.
async function openStashDiff(options) {
  const { vscode, links, repo, repoToken, identity, oid, index, file, contents, current } = options;
  if (!links || typeof links.createVerified !== 'function' || typeof links.validateStaticIdentity !== 'function' ||
      !REPO_IDENTITY.test(identity || '') || !isStashOid(oid) || !relativeFile(file?.path) || !contents) {
    throw new Error('The configured Gitpane backend does not provide verified stash contents. Update the backend and try again.');
  }
  const binary = [contents.old, contents.new].find(side => side?.kind === 'binary');
  if (binary) throw new Error(`Gitpane cannot show a text diff for the saved binary file ${file.path}.`);
  const repoPath = repo.rootUri.fsPath;
  const { left: leftPath, right: rightPath } = stashSides(file);
  if (leftPath === null && rightPath === null) throw new Error('The saved stash file has no comparison sides.');
  const leftText = sideText(contents.old), rightText = sideText(contents.new);
  const display = vscode.Uri.file(stashFilePath(repoPath, rightPath || leftPath));
  const leftUri = leftPath === null ? display : vscode.Uri.file(stashFilePath(repoPath, leftPath));
  const empty = display.with({ scheme: 'gitpane-empty', query: '' });
  if (!current()) throw new Error('The selected stash is no longer available. Refresh and try again.');
  if (!await links.validateStaticIdentity(repoPath, identity, current)) {
    throw new Error('The repository changed since the stash was saved. Refresh and try again.');
  }
  const guard = async () => {
    if (!current()) return false;
    return links.validateStaticIdentity(repoPath, identity, current);
  };
  // The immutable stash OID belongs in the document group: `index` is display
  // only and can be reused by a different snapshot after a new stash is pushed.
  const group = JSON.stringify([repoPath, identity, repoToken, oid, index, file.source, file.status, file.old_path, file.path]);
  const side = async (uri, text, name) => {
    if (uri === null) return empty;
    return links.createVerified(uri, async () => text, guard, current, JSON.stringify([group, name]));
  };
  const left = await side(leftPath === null ? null : leftUri, leftText, 'left');
  const right = await side(rightPath === null ? null : display, rightText, 'right');
  if (!current()) throw new Error('The selected stash is no longer available. Refresh and try again.');
  await vscode.commands.executeCommand('vscode.diff', left, right, `${display.fsPath} (stash@{${index}})`);
}

module.exports = {
  isStashOid, openStashDiff, stashErrorMessage, stashFileLabel, stashSides,
  stashUnavailableMessage, validateStashContent, validateStashList
};
