'use strict';

const path = require('node:path');
const { relativeFile, scopedFile, validRecord } = require('./change-pager');

const oid = value => typeof value === 'string' && /^[a-f0-9]{40,64}$/.test(value);
const identity = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

function isPointerComparison(record) {
  return !!record?.is_submodule && (record.group === 'merge' ||
    (record.group === 'index' && ['index_renamed', 'index_copied', 'index_type_changed', 'index_deleted'].includes(record.kind)) ||
    (record.group === 'working' && ['type_changed', 'working_deleted'].includes(record.kind)));
}

function validateSnapshot(value, record, expectedIdentity) {
  const names = record.group === 'merge' ? ['base', 'ours', 'theirs'] :
    record.group === 'working' ? ['index', 'working'] : ['base', 'index'];
  if (!value || value.repo_identity !== expectedIdentity || !(value.head_oid === null || oid(value.head_oid)) ||
      !validRecord(value.record) || JSON.stringify(value.record) !== JSON.stringify(record) || !value.sides ||
      Object.keys(value.sides).sort().join(',') !== [...names].sort().join(',')) {
    throw new Error('Gitpane returned invalid submodule pointer details.');
  }
  for (const [name, side] of Object.entries(value.sides)) {
    if (side === null) continue;
    if (name === 'working' && record.group === 'working' && side?.kind === 'working_file' &&
        relativeFile(side.path) && identity(side.token)) continue;
    if (!side || !relativeFile(side.path) || !oid(side.oid) || !Number.isInteger(side.mode) ||
        !((side.kind === 'gitlink' && side.mode === 57344) || (side.kind === 'blob' && (side.mode & 61440) === 32768) ||
          (side.kind === 'symlink' && side.mode === 40960))) {
      throw new Error('Submodule pointer comparisons with directories or unsupported file types are unavailable.');
    }
  }
  if (record.group === 'working' && (value.sides.index?.kind !== 'gitlink' ||
      (record.kind === 'working_deleted' ? value.sides.working !== null : value.sides.working?.kind !== 'working_file'))) {
    throw new Error('Gitpane returned invalid working submodule pointer details.');
  }
  if (!Object.values(value.sides).some(side => side?.kind === 'gitlink')) throw new Error('The selected change no longer contains a submodule pointer.');
  return value;
}

function selectedSides(record, sides) {
  if (record.group === 'index') return ['base', 'index'];
  if (record.group === 'working') return ['index', 'working'];
  if (sides.ours && sides.theirs) return ['ours', 'theirs'];
  if (sides.base && sides.ours) return ['base', 'ours'];
  if (sides.base && sides.theirs) return ['base', 'theirs'];
  if (sides.base) return ['base', null];
  if (sides.ours) return [null, 'ours'];
  if (sides.theirs) return [null, 'theirs'];
  throw new Error('The conflict has no available Git stages.');
}

async function openPointerDiff(node, provider, tree, backend, links, vscode, options = {}) {
  const repo = node.repo, token = node.repoToken, record = structuredClone(node.record);
  if (!isPointerComparison(record)) throw new Error('This submodule pointer comparison is unavailable.');
  const status = tree.backendStatuses?.get(repo.rootUri.toString());
  const repoIdentity = options.repoIdentity || status?.repo_identity;
  if (!identity(repoIdentity)) throw new Error('Repository identity is unavailable. Refresh Gitpane and try again.');
  const current = () => {
    if (!tree.git.repositories.some(item => item.rootUri.toString() === repo.rootUri.toString()) ||
        !tree.isCurrentRepository(repo, token) || (options.current && !options.current())) return false;
    if (!node.parentWorktree) return true;
    const parent = tree.git.repositories.find(item => item.rootUri.toString() === node.parentWorktree.repoKey);
    return !!parent && tree.isCurrentRepository(parent, node.parentWorktree.repoToken) &&
      tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) &&
      tree.worktreeRepository(parent, node.parentWorktree.path)?.rootUri.toString() === repo.rootUri.toString();
  };
  const selected = () => current() && !!provider.currentChange(node);
  if (!selected()) throw new Error('The selected submodule change is no longer available.');
  const repoPath = repo.rootUri.fsPath;
  if (options.verifyAncestors && !await options.verifyAncestors()) throw new Error('The parent submodule changed.');
  if (!selected()) throw new Error('The selected submodule change is no longer available.');
  const snapshot = validateSnapshot(await backend.submodulePointerCompare(repoPath, record, repoIdentity), record, repoIdentity);
  if (!selected()) throw new Error('The selected submodule change is no longer available.');
  const expected = JSON.stringify([snapshot.repo_identity, snapshot.head_oid, snapshot.record, snapshot.sides]);
  let pending;
  const verify = () => {
    if (!current()) return Promise.resolve(false);
    if (pending) return pending;
    pending = (async () => {
      if (options.verifyAncestors && !await options.verifyAncestors()) return false;
      if (!current()) return false;
      const latest = validateSnapshot(await backend.submodulePointerCompare(repoPath, record, repoIdentity), record, repoIdentity);
      if (!current() || JSON.stringify([latest.repo_identity, latest.head_oid, latest.record, latest.sides]) !== expected) return false;
      if (options.verifyAncestors && !await options.verifyAncestors()) return false;
      return current();
    })();
    pending.finally(() => { pending = undefined; }).catch(() => {});
    return pending;
  };
  // Deleted submodules can have a regular file blocking an ancestor. Both
  // comparison sides are virtual and never open that path in the filesystem.
  const virtualPath = relative => record.kind === 'working_deleted' ?
    path.join(repoPath, ...relative.split('/')) : scopedFile(repoPath, relative);
  const uri = vscode.Uri.file(virtualPath(record.path));
  const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
  const group = JSON.stringify([repoPath, repoIdentity, token, record, snapshot.head_oid, snapshot.sides, options.ancestorKey]);
  const batch = { repo: repoPath, identity: repoIdentity, snapshot: { record, head_oid: snapshot.head_oid, sides: snapshot.sides },
    ...(options.verifyAncestors ? { ancestorKey: options.ancestorKey, verifyAncestors: options.verifyAncestors } : {}) };
  const makeSide = async name => {
    if (!name || !snapshot.sides[name]) return empty;
    const target = vscode.Uri.file(virtualPath(snapshot.sides[name].path));
    const read = async () => {
      const result = await backend.submodulePointerContent(repoPath, record, repoIdentity, snapshot.head_oid, snapshot.sides, name);
      if (!result || typeof result.content !== 'string') throw new Error('Gitpane returned invalid submodule pointer contents.');
      return result.content;
    };
    return links.createVerified(target, read, verify, current, group, batch);
  };
  const [leftName, rightName] = selectedSides(record, snapshot.sides);
  const left = await makeSide(leftName), right = await makeSide(rightName);
  if (!selected() || !await provider.revalidateChange(node.fileId) || !selected() || !await verify() || !selected()) {
    throw new Error('The selected submodule pointer changed. Refresh and try again.');
  }
  await vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(record.path)} (${record.group})`);
}

module.exports = { isPointerComparison, openPointerDiff, selectedSides, validateSnapshot };
