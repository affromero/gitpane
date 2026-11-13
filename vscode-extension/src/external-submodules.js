'use strict';

const path = require('node:path');
const { relativeFile } = require('./change-pager');

const token = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

// External child owners use the same guarded read channel as their worktree.
function externalSubmodules(provider, vscode, options) {
  const { item, repoKey, repoToken, worktreeId, summary, live, groupsFor, pageFor } = options;
  const tree = provider.tree;
  const anchorIdentity = tree.backendStatuses.get(repoKey)?.repo_identity;
  const rootIdentity = tree.directoryIdentity(item.path);
  const valid = (child, parent, parentIdentity, prefix) => {
    if (!child || !relativeFile(child.path) || !Array.isArray(child.chain) ||
        child.chain.length !== prefix.length + 1 || child.chain.length > 32 ||
        JSON.stringify(child.chain.slice(0, -1)) !== JSON.stringify(prefix) ||
        !token(child.status?.repo_identity) || !Array.isArray(child.submodules)) return false;
    const link = child.chain.at(-1);
    const expected = path.resolve(parent, ...child.path.split('/'));
    const identity = typeof child.repo === 'string' && path.isAbsolute(child.repo) && tree.directoryIdentity(expected);
    return !!identity && identity.path === tree.directoryIdentity(child.repo)?.path &&
      link?.path === child.path && link.parent_identity === parentIdentity && link.child_identity === child.status.repo_identity;
  };
  const verify = (children, parent, parentIdentity, prefix, budget) => {
    for (const child of children) {
      if (--budget.remaining < 0 || !valid(child, parent, parentIdentity, prefix))
        throw new Error('Gitpane returned invalid worktree submodule details.');
      verify(child.submodules, child.repo, child.status.repo_identity, child.chain, budget);
    }
  };
  const build = child => {
    const identity = tree.directoryIdentity(child.repo);
    const key = JSON.stringify(['external-submodule', repoKey, repoToken, item.path, child.chain]);
    if (tree.hiddenRepositories?.isHidden(identity, child.status.repo_identity) ||
        (tree.submoduleDiscovery?.enabled === false && !provider.externalOwners.has(provider.ids.get(key)))) return undefined;
    const id = provider.idFor(key, live);
    let owner = provider.externalOwners.get(id);
    if (!owner) {
      const selection = { repo: tree.git.repositories.find(repo => repo.rootUri.toString() === repoKey)?.rootUri.fsPath,
        path: item.path, expected_repo_identity: anchorIdentity, expected_worktree_identity: item.identity,
        chain: child.chain };
      const guard = () => {
        const current = provider.resolveWorktree(worktreeId);
        const now = tree.directoryIdentity(child.repo), rootNow = tree.directoryIdentity(item.path);
        return !provider.disposed && !!vscode.workspace.isTrusted && !!current?.external &&
          current.externalIdentity === item.identity && current.repoIdentity === anchorIdentity &&
          rootNow?.path === rootIdentity?.path && rootNow?.dev === rootIdentity?.dev && rootNow?.ino === rootIdentity?.ino &&
          now?.path === identity.path && now?.dev === identity.dev && now?.ino === identity.ino;
      };
      const repo = { rootUri: vscode.Uri.file(child.repo), state: { submodules: [] }, externalToken: id };
      owner = { repo, status: child.status, guard, selection, submodule: true };
      provider.externalOwners.set(id, owner);
      tree.backend.externalSelections.set(child.repo, { selection, guard });
    } else {
      owner.status = child.status;
    }
    const status = child.status;
    return { id, name: path.basename(child.repo), branch: status.branch || 'unborn', submodule: true,
      dirty: !!status.is_dirty, ahead: status.ahead || 0, behind: status.behind || 0,
      worktreeCount: 0, worktrees: [], stashes: [], stashCount: 0,
      hasDirtySubmodules: !!status.has_dirty_submodules, hasUnpushedSubmodules: !!status.has_unpushed_submodules,
      changeCountLowerBound: status.file_count_lower_bound || 0, changesPage: pageFor(id),
      groups: groupsFor(owner.repo, id), submodules: child.submodules.map(build).filter(Boolean) };
  };
  if (!rootIdentity || !token(anchorIdentity) || !token(item.identity) || !token(summary?.repo_identity) || !Array.isArray(summary?.checkout_submodules)) return [];
  try {
    verify(summary.checkout_submodules, item.path, summary.repo_identity, [], { remaining: 128 });
    return summary.checkout_submodules.map(build).filter(Boolean);
  }
  catch (error) { options.errors.push(`Could not inspect submodules in ${item.path}: ${error.message}`); return []; }
}

module.exports = { externalSubmodules };
