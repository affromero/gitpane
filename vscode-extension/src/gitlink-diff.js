'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

function validGitlink(value) {
  const side = item => item === null || (!!item && typeof item === 'object' &&
    typeof item.oid === 'string' && /^[a-f0-9]{40,64}$/.test(item.oid) && Number.isInteger(item.mode) &&
    ((item.kind === 'gitlink' && item.mode === 57344) || (item.kind === 'file' && (item.mode & 61440) === 32768) ||
     (item.kind === 'symlink' && item.mode === 40960) || (item.kind === 'tree' && item.mode === 16384)));
  return !!value && side(value.old) && side(value.new) && (value.old?.kind === 'gitlink' || value.new?.kind === 'gitlink');
}

async function openGitlinkDiff(options) {
  const { vscode, links, repo, repoIdentity, repoToken, file, oldRef, newRef, current, verify, title, readHistorical,
    verifyAncestors, ancestorKey, contextKey } = options;
  if (!links || !validGitlink(file.gitlink) || !/^[a-f0-9]{64}$/.test(repoIdentity || '')) {
    throw new Error('The configured Gitpane backend does not provide verified submodule pointer details. Update the backend and try again.');
  }
  if (file.gitlink.old?.kind === 'tree' || file.gitlink.new?.kind === 'tree') {
    throw new Error('Submodule changes to or from a directory cannot be opened as a file diff.');
  }
  if (verifyAncestors && (typeof ancestorKey !== 'string' || !ancestorKey)) {
    throw new Error('The submodule ancestor guard is unavailable.');
  }
  if (!relativeFile(file.path) || (file.status === 'R' && !relativeFile(file.oldPath))) {
    throw new Error('The selected submodule path is invalid.');
  }
  if (!await verify()) throw new Error('The selected submodule change is no longer available.');
  const uri = vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
  const oldUri = file.status === 'R' ? vscode.Uri.file(path.join(repo.rootUri.fsPath, file.oldPath)) : uri;
  const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
  const documentGuard = async () => {
    if (verifyAncestors && !await verifyAncestors()) return false;
    if (!await links.validateStaticIdentity(repo.rootUri.fsPath, repoIdentity, current)) return false;
    if (verifyAncestors && !await verifyAncestors()) return false;
    return current();
  };
  const group = JSON.stringify([repo.rootUri.fsPath, repoIdentity, repoToken, contextKey, ...(verifyAncestors ? [ancestorKey] : [])]);
  const side = async (value, target, ref, relative) => {
    if (!value) return empty;
    if (value.kind === 'gitlink') return links.createStatic(target, `Subproject commit ${value.oid}\n`, documentGuard, current, group);
    if (typeof readHistorical !== 'function' || typeof repo.buffer !== 'function' ||
        typeof ref !== 'string' || !/^[a-f0-9]{40,64}$/.test(ref)) {
      throw new Error('The configured Gitpane backend does not provide verified historical file contents. Update the backend and try again.');
    }
    const metadata = () => readHistorical(ref, relative, value.oid, value.mode);
    const sideGroup = JSON.stringify([group, ref, relative, value.oid, value.mode]);
    return links.createBinary(target, metadata, () => repo.buffer(ref, relative), documentGuard, current, sideGroup);
  };
  const left = await side(file.gitlink.old, oldUri, oldRef, file.status === 'R' ? file.oldPath : file.path);
  const right = await side(file.gitlink.new, uri, newRef, file.path);
  if (!await verify()) throw new Error('The selected submodule change is no longer available.');
  await vscode.commands.executeCommand('vscode.diff', left, right, title);
}

module.exports = { openGitlinkDiff };
