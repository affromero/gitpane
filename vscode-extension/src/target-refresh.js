'use strict';

async function refreshTarget(provider, id, kind, guard) {
  if (!guard()) return [];
  if (kind === 'repo' && provider.externalOwners.get(id)?.submodule) {
    const repo = provider.currentOwner(id);
    if (!repo) return [];
    await provider.tree.refresh();
    if (guard() && provider.currentOwner(id)) await provider.loadChanges(id);
    return guard() ? [repo.rootUri.toString()] : [];
  }
  const selected = kind === 'worktree' ? provider.worktrees.get(id) : provider.repositories.get(id);
  const target = kind === 'worktree' ? provider.currentWorktreeRepository(id) : provider.currentRepository(id);
  const anchor = kind === 'worktree'
    ? provider.tree.git.repositories.find(repo => repo.rootUri.toString() === selected?.repoKey)
    : target && provider.tree.explicitWorktreeAnchor?.(target)?.anchor;
  const primary = kind === 'worktree' ? anchor : target;
  if (!selected || !primary) return [];
  const primaryToken = provider.tree.repositoryToken(primary);
  const targetToken = target && provider.tree.repositoryToken(target);
  const anchorToken = anchor && provider.tree.repositoryToken(anchor);
  const generation = provider.tree.backendGeneration;
  const anchorKey = anchor?.rootUri.toString();
  const current = () => {
    if (provider.disposed || generation !== provider.tree.backendGeneration ||
        !provider.tree.isCurrentRepository(primary, primaryToken)) return false;
    if (kind === 'repo') {
      if (provider.currentRepository(id)?.rootUri.toString() !== primary.rootUri.toString()) return false;
      if (!anchor) return true;
      const link = provider.tree.explicitWorktreeAnchor?.(primary);
      return !!link && link.anchor.rootUri.toString() === anchorKey &&
        provider.tree.isCurrentRepository(anchor, anchorToken);
    }
    const link = provider.worktrees.get(id);
    if (!link || link.repoKey !== selected.repoKey || link.repoToken !== selected.repoToken ||
        link.path !== selected.path || !provider.tree.workspaceWorktrees(primary).some(item => item.path === selected.path)) return false;
    if (!target) return !link.childRepoId;
    return link.childToken === targetToken && provider.currentWorktreeRepository(id)?.rootUri.toString() === target.rootUri.toString();
  };
  const repos = [...new Set([primary, target, anchor].filter(Boolean))];
  if (!current()) return [];
  const result = await provider.tree.refreshTargets(repos, current);
  if (result?.scopeChanged) {
    await provider.refresh();
    return [];
  }
  if (!current()) return [];
  for (const pageId of [...provider.pager.entries.keys()]) {
    const owner = provider.currentOwner(pageId);
    if (!owner || !repos.some(repo => repo.rootUri.toString() === owner.rootUri.toString())) continue;
    const pending = provider.pager.get(pageId)?.pending;
    if (pending) await pending;
    if (!current()) return [];
    if (provider.currentOwner(pageId)?.rootUri.toString() === owner.rootUri.toString()) await provider.loadChanges(pageId);
    if (!current()) return [];
  }
  return repos.map(repo => repo.rootUri.toString());
}

module.exports = { refreshTarget };
