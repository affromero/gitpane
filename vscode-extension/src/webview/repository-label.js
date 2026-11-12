'use strict';

window.GitpaneRepositoryLabel = (repo, count) => {
  const details = [repo.name, repo.submodule ? 'submodule' : '', repo.worktree ? 'worktree' : '', repo.branch || 'unknown branch',
    repo.statusUnavailable ? 'status unavailable' : repo.dirty ? 'has changes' : 'clean',
    repo.statusUnavailable ? '' : count];
  if (!repo.statusUnavailable) {
    if (repo.stashCount > 0) details.push(`${repo.stashCount} ${repo.stashCount === 1 ? 'stash' : 'stashes'}`);
    if (repo.worktreeCount > 0) details.push(`${repo.worktreeCount} ${repo.worktreeCount === 1 ? 'worktree' : 'worktrees'}`);
    if (repo.ahead > 0) details.push(`${repo.ahead} ahead`);
    if (repo.behind > 0) details.push(`${repo.behind} behind`);
    if (repo.hasDirtySubmodules) details.push('dirty submodules');
    if (repo.hasUnpushedSubmodules) details.push('submodules need attention');
  }
  if (repo.live) details.push('live session');
  if (repo.pinned) details.push('pinned submodule');
  return details.filter(Boolean).join(', ');
};

window.GitpaneSubmoduleParts = file => {
  if (!file.submodule || !file.submoduleDetails) return [];
  const info = file.submoduleDetails;
  const parts = [];
  const state = { modified: ['+commit', 'commit changed'], uninitialized: ['-uninit', 'uninitialized'], dirty: ['~dirty', 'dirty'] }[info.state];
  if (state) parts.push([state[0], 'submodule-bracket', state[1]]);
  if (info.state === 'uninitialized') return parts;
  if (info.head) {
    const branch = info.head.detached ? 'detached' : info.head.branch.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ');
    parts.push([`@${branch}`, 'submodule-branch', `branch ${branch}`]);
  }
  if (info.warn.pointer_unreachable) parts.push(['⚠unreach', 'submodule-unreachable', 'pointer unreachable']);
  else {
    if (info.warn.unpushed_commits > 0) parts.push([`↑${info.warn.unpushed_commits}`, 'submodule-unpushed', `${info.warn.unpushed_commits} unpushed commits`]);
    if (info.warn.needs_merge_to_default) parts.push(['↛main', 'submodule-needs-merge', 'needs merge to default branch']);
  }
  return parts;
};

window.GitpaneSubmoduleToggle = (document, owner, open, toggle) => {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'worktree submodule-action';
  button.textContent = `${open ? '▾' : '▸'}◈${owner.submodules.length}`;
  button.setAttribute('aria-label', `${open ? 'Collapse' : 'Expand'} submodules for ${owner.name}`);
  button.setAttribute('aria-expanded', String(open));
  button.addEventListener('click', event => { event.stopPropagation(); toggle(); });
  return button;
};
