'use strict';

const path = require('node:path');
const fs = require('node:fs');

// Values from the version 1 API of VS Code's bundled Git extension.
const Status = Object.freeze({
  INDEX_MODIFIED: 0, INDEX_ADDED: 1, INDEX_DELETED: 2,
  INDEX_RENAMED: 3, INDEX_COPIED: 4, MODIFIED: 5, DELETED: 6,
  UNTRACKED: 7, IGNORED: 8, INTENT_TO_ADD: 9, INTENT_TO_RENAME: 10,
  TYPE_CHANGED: 11
});

function groups(state) {
  const working = state.workingTreeChanges || [];
  return [
    { id: 'merge', label: 'Conflicts', changes: state.mergeChanges || [] },
    { id: 'index', label: 'Staged', changes: state.indexChanges || [] },
    { id: 'working', label: 'Unstaged', changes: working.filter(c => c.status !== Status.UNTRACKED) },
    { id: 'untracked', label: 'Untracked', changes: [...working.filter(c => c.status === Status.UNTRACKED), ...(state.untrackedChanges || [])] }
  ].filter(group => group.changes.length);
}

function changedFileCount(changeGroups) {
  const files = new Set();
  for (const group of changeGroups) {
    for (const change of group.changes) files.add(change.uri.toString());
  }
  return files.size;
}

function statusLabel(status) {
  if ([Status.INDEX_ADDED, Status.INTENT_TO_ADD].includes(status)) return 'A';
  if ([Status.INDEX_DELETED, Status.DELETED].includes(status)) return 'D';
  if ([Status.INDEX_RENAMED, Status.INDEX_COPIED, Status.INTENT_TO_RENAME].includes(status)) return 'R';
  if (status === Status.UNTRACKED) return '?';
  if (status > Status.TYPE_CHANGED) return 'C';
  return 'M';
}

// Describe each side independently so the caller can use VS Code's Git URI
// provider for revisions and an empty document for absent files.
function comparison(group, change) {
  const current = change.uri;
  const original = change.originalUri || current;
  if (group === 'merge') return { open: current };
  if (group === 'untracked' || change.status === Status.UNTRACKED) {
    return { left: { empty: true }, right: { file: current } };
  }
  if (group === 'index') {
    const added = change.status === Status.INDEX_ADDED;
    const deleted = change.status === Status.INDEX_DELETED;
    return {
      left: added ? { empty: true } : { git: original, ref: 'HEAD' },
      right: deleted ? { empty: true } : { git: current, ref: '' }
    };
  }
  return {
    left: change.status === Status.INTENT_TO_ADD ? { empty: true } : { git: current, ref: '' },
    right: change.status === Status.DELETED ? { empty: true } : { file: current }
  };
}

function sameOrInside(parent, child) {
  const relative = path.relative(parent, child);
  return relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function canonicalPath(value) {
  let result;
  try { result = fs.realpathSync.native(value); }
  catch { result = path.resolve(value); }
  return process.platform === 'win32' ? result.toLowerCase() : result;
}

function visibleRepository(repoPath, workspacePaths, identify = canonicalPath) {
  const canonical = identify(repoPath);
  return workspacePaths.some(value => {
    const workspace = identify(value);
    return sameOrInside(workspace, canonical);
  });
}

function isSubmodule(repoPath, filePath, submodules) {
  return (submodules || []).some(submodule => {
    const modulePath = path.resolve(repoPath, submodule.path);
    return sameOrInside(modulePath, filePath) && sameOrInside(filePath, modulePath);
  });
}

module.exports = { Status, canonicalPath, changedFileCount, comparison, groups, isSubmodule, statusLabel, visibleRepository };
