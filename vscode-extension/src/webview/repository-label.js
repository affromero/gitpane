'use strict';

window.GitpaneStatusClass = status =>
  ({ M: 'modified', A: 'added', D: 'deleted', R: 'renamed', '?': 'untracked', C: 'conflicted', '!': 'behind' })[status] || 'muted';

window.GitpaneStatusHelp = status => ({
  M: 'Modified file or changed file type',
  A: 'Added file',
  D: 'Deleted file',
  R: 'Renamed or copied file',
  '?': 'Untracked path',
  C: 'Unresolved conflict',
  '!': 'Submodule needs attention',
})[status] || 'Changed file';

window.GitpaneAttentionHelp = 'Submodules needing attention. A submodule branch may have unpushed commits, or the commit recorded in the parent index may be missing from known remote branches or from the merge target.';

window.GitpaneGroupHelp = group => ({
  merge: 'Files with unresolved merge conflicts.',
  index: 'Changes staged for the next commit.',
  working: 'Changes in tracked files that are outside the staging area.',
  untracked: 'Files or directories Git is not tracking.',
  attention: window.GitpaneAttentionHelp,
})[group];

const upstreamHelp = (count, direction) =>
  `${count} ${count === 1 ? 'commit' : 'commits'} ${direction} the configured upstream branch`;

window.GitpaneDefaultBranchLagLabel = lag => lag?.behind > 0
  ? `${lag.behind} ${lag.behind === 1 ? 'commit' : 'commits'} behind ${lag.branch} (remote default branch)`
  : '';

window.GitpaneAddGitStatus = (add, tail, owner) => {
  if (owner.ahead > 0) add(tail, 'ahead', `↑${owner.ahead}`, upstreamHelp(owner.ahead, 'ahead of'));
  if (owner.behind > 0) add(tail, 'behind', `↓${owner.behind}`, upstreamHelp(owner.behind, 'behind'));
  const lagLabel = window.GitpaneDefaultBranchLagLabel(owner.defaultBranchLag);
  if (lagLabel) {
    const { behind, short_branch, branch } = owner.defaultBranchLag;
    add(tail, 'behind', `↓${behind} ${short_branch || branch}`, lagLabel);
  }
  if (owner.hasDirtySubmodules) add(tail, 'dirty-submodule', '◈', 'One or more submodules are uninitialized, have local changes, or are checked out at a different commit.');
  if (owner.hasUnpushedSubmodules) add(tail, 'unpushed-submodule', '⇡', window.GitpaneAttentionHelp + '\nSee Submodules needing attention in the change list for details.');
};

window.GitpaneRepositoryLabel = (repo, count) => {
  const details = [repo.name, repo.submodule ? 'submodule' : '', repo.worktree ? 'worktree' : '', repo.branch || 'unknown branch',
    repo.statusUnavailable ? 'status unavailable' : repo.dirty ? 'has changes' : 'clean',
    repo.statusUnavailable ? '' : count];
  if (!repo.statusUnavailable) {
    if (repo.stashCount > 0) details.push(`${repo.stashCount} ${repo.stashCount === 1 ? 'stash' : 'stashes'}`);
    if (repo.worktreeCount > 0) details.push(`${repo.worktreeCount} ${repo.worktreeCount === 1 ? 'worktree' : 'worktrees'}`);
    if (repo.ahead > 0) details.push(`${repo.ahead} ahead`);
    if (repo.behind > 0) details.push(`${repo.behind} behind`);
    details.push(window.GitpaneDefaultBranchLagLabel(repo.defaultBranchLag));
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
  const state = {
    modified: ['+commit', 'commit changed', 'The submodule commit changed in the parent index or in the local checkout.'],
    uninitialized: ['-uninit', 'uninitialized', 'The submodule has no initialized local checkout.'],
    dirty: ['~dirty', 'dirty', 'The submodule checkout contains uncommitted changes.'],
  }[info.state];
  if (state) parts.push([state[0], 'submodule-bracket', state[1], state[2]]);
  if (info.state === 'uninitialized') return parts;
  if (info.head) {
    const branch = info.head.detached ? 'detached' : info.head.branch.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ');
    parts.push([`@${branch}`, 'submodule-branch', `branch ${branch}`,
      info.head.detached ? 'Detached HEAD. The submodule is checked out at a commit without a current branch.' : `Checked-out submodule branch: ${branch}`]);
  }
  if (info.warn.pointer_unreachable) parts.push(['⚠unreach', 'submodule-unreachable', 'pointer unreachable',
    'No known remote-tracking branch contains the submodule commit recorded in the parent index. Other clones may be unable to fetch it.']);
  else {
    if (info.warn.unpushed_commits > 0) parts.push([`↑${info.warn.unpushed_commits}`, 'submodule-unpushed', `${info.warn.unpushed_commits} unpushed commits`,
      upstreamHelp(info.warn.unpushed_commits, 'ahead of') + ' in the submodule.']);
    if (info.warn.needs_merge_to_default) parts.push(['↛merge', 'submodule-needs-merge', 'needs merge to target branch',
      'The commit recorded in the parent index needs to reach the branch declared in .gitmodules, or the remote default branch if no branch is declared.']);
  }
  return parts;
};

window.GitpaneSubmoduleToggle = (document, owner, open, toggle) => {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'worktree submodule-action';
  button.textContent = `${open ? '▾' : '▸'}◈${owner.submodules.length}`;
  button.title = `${open ? 'Collapse' : 'Expand'} ${owner.submodules.length} ${owner.submodules.length === 1 ? 'submodule' : 'submodules'}`;
  button.setAttribute('aria-label', `${open ? 'Collapse' : 'Expand'} submodules for ${owner.name}`);
  button.setAttribute('aria-expanded', String(open));
  button.addEventListener('click', event => { event.stopPropagation(); toggle(); });
  return button;
};
