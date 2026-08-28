use crate::config::SubmoduleConfig;
use git2::Repository;
use std::path::{Path, PathBuf};

mod change_summary;
mod submodule;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_fetch_kill;
#[cfg(test)]
mod tests_refs;
#[cfg(test)]
mod tests_submodule;
mod worktree;

use change_summary::{ChangeSummary, collect_change_summary};
use worktree::{collect_worktree_info, compute_ahead_behind, fetch_remote_silent};
pub(crate) use worktree::{kill_in_flight_git_ops, run_git_op_capturing};

#[derive(Clone, Debug)]
pub(crate) struct RepoStatus {
    pub branch: String,
    pub head_oid: Option<String>,
    pub files: Vec<FileEntry>,
    pub ahead: usize,
    pub behind: usize,
    pub is_dirty: bool,
    /// Linked worktrees (excludes the main working tree)
    pub worktree_info: Vec<WorktreeEntry>,
    /// True when .gitmodules exists (repo uses submodules)
    pub has_submodules: bool,
    pub submodules: Vec<SubmoduleInfo>,
    pub has_dirty_submodules: bool,
    /// Any submodule has unpushed_commits>0 OR pointer_unreachable
    pub has_unpushed_submodules: bool,
    /// True when the last `git fetch` failed (auth, network, timeout)
    pub fetch_failed: bool,
    /// Local stash entries (oldest at index 0; `stash@{n}` matches by index).
    pub stashes: Vec<StashEntry>,
    /// Fingerprint of the ref tips the graph renders (branches + tags), bucketed
    /// so the change check can be scoped to the active `BranchFilter`. Lets the
    /// graph reload when a commit lands on a branch that is not the root's
    /// checked-out HEAD (e.g. a commit made in a linked worktree, whose branch
    /// shares the root's `refs/heads/*`).
    pub refs: RefsFingerprint,
}

/// Order-independent hashes of the rendered ref tips, split by category so the
/// reload check can compare only what the current `BranchFilter` actually draws.
/// Each bucket XORs `hash(name, oid)` over its refs: order-free, allocation-free,
/// and O(ref count) with no sort. Names are included so a new branch/tag at an
/// existing commit still registers as a change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RefsFingerprint {
    pub local: u64,
    pub remote: u64,
    pub tags: u64,
}

impl RepoStatus {
    pub fn stash_count(&self) -> usize {
        self.stashes.len()
    }
}

/// Snapshot the tips of the refs the graph renders (`refs/heads/*`,
/// `refs/remotes/*`, `refs/tags/*`), mirroring `git::graph::resolve_refs`:
/// annotated tags are peeled to their commit. Symbolic refs (e.g.
/// `refs/remotes/origin/HEAD`) have no direct target and are skipped, matching
/// the graph's branch iteration. Reuses the already-open repo, so it costs one
/// extra ref-db pass on a poll that already walks full status.
fn graph_refs_fingerprint(repo: &Repository) -> RefsFingerprint {
    use std::hash::{Hash, Hasher};

    fn mix(acc: &mut u64, name: &str, oid: git2::Oid) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut hasher);
        oid.as_bytes().hash(&mut hasher);
        *acc ^= hasher.finish();
    }

    let mut fp = RefsFingerprint::default();
    let Ok(references) = repo.references() else {
        return fp;
    };
    for reference in references {
        let Ok(reference) = reference else {
            continue;
        };
        let Ok(name) = reference.name() else {
            continue;
        };
        if name.starts_with("refs/heads/") {
            if let Some(oid) = reference.target() {
                mix(&mut fp.local, name, oid);
            }
        } else if name.starts_with("refs/remotes/") {
            if let Some(oid) = reference.target() {
                mix(&mut fp.remote, name, oid);
            }
        } else if name.starts_with("refs/tags/") {
            // Peel annotated tags to the commit the graph labels, falling back
            // to the raw target for lightweight tags.
            let oid = reference
                .peel_to_commit()
                .ok()
                .map(|commit| commit.id())
                .or_else(|| reference.target());
            if let Some(oid) = oid {
                mix(&mut fp.tags, name, oid);
            }
        }
    }
    fp
}

/// One entry in a repo's stash list, mirroring `git stash list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StashEntry {
    /// Position in the stash list (matches `stash@{index}`).
    pub index: usize,
    /// Stash message, e.g. `"WIP on main: 1234abcd Initial"`.
    pub message: String,
    /// Hex oid of the stash commit, for downstream diff/show.
    pub oid: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    pub status: FileStatus,
    /// Index/staged side is dirty (any `is_index_*`); the row can be unstaged.
    pub staged: bool,
    /// Worktree/unstaged side is dirty (any `is_wt_*`, including untracked); the
    /// row can be staged. `status` collapses both sides for display; these two
    /// keep them apart so a context menu can offer Stage vs Unstage correctly.
    pub unstaged: bool,
    pub is_submodule: bool,
    pub submodule_state: Option<SubmoduleState>,
    pub submodule_warn: SubmoduleWarn,
    /// Checked-out branch of the submodule (or detached). `None` for non
    /// submodule rows and for submodules that could not be opened.
    pub submodule_head: Option<SubmoduleHead>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SubmoduleState {
    Modified,
    Uninitialized,
    Dirty,
}

/// The submodule's currently checked-out HEAD. After `git submodule update`
/// a submodule sits on a detached HEAD by default, so `Detached` is common and
/// itself a useful signal: a modification there is not on any tracked branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SubmoduleHead {
    Branch(String),
    Detached,
}

/// "You owe a push" warnings for a submodule. Orthogonal to `SubmoduleState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubmoduleWarn {
    /// Submodule HEAD branch ahead of its upstream (0 if no upstream).
    pub unpushed_commits: usize,
    /// Parent's recorded oid is not reachable from any of the submodule's
    /// `refs/remotes/*` refs — committing the parent would pin a sha nobody
    /// else can fetch.
    pub pointer_unreachable: bool,
    /// Parent's recorded oid IS on a remote, but not on the submodule's default
    /// branch (`origin/HEAD`, falling back to `origin/main`/`origin/master`),
    /// so it still needs a merge/PR there. Stays `false` when no default branch
    /// resolves, so it never fires on a false positive.
    pub needs_merge_to_default: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct WorktreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub is_dirty: bool,
    pub file_count: usize,
    pub has_dirty_submodules: bool,
    pub has_unpushed_submodules: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct SubmoduleInfo {
    pub name: String,
    pub path: PathBuf,
    pub state: Option<SubmoduleState>,
    pub head: Option<SubmoduleHead>,
    pub head_oid: Option<String>,
    pub workdir_oid: Option<String>,
    pub warn: SubmoduleWarn,
}

/// Fast local-only status query (no network). Used by filesystem watcher refreshes.
pub(crate) fn query_status(
    path: &Path,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, false, false, sub_cfg)
}

/// Status query with `git fetch` first. Used by explicit user refresh (`r` key).
pub(crate) fn query_status_with_fetch(
    path: &Path,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, true, true, sub_cfg)
}

fn query_status_inner(
    path: &Path,
    fetch: bool,
    recurse_untracked_dirs: bool,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    let mut repo = Repository::open(path)?;

    // Stash entries: requires &mut, so do this before any immutable borrow of `repo`.
    let mut stashes: Vec<StashEntry> = Vec::new();
    let _ = repo.stash_foreach(|index, message, oid| {
        stashes.push(StashEntry {
            index,
            message: message.to_string(),
            oid: oid.to_string(),
        });
        true
    });

    // Branch name and current HEAD oid
    let (branch, head_oid) = match repo.head() {
        Ok(reference) => {
            let oid = reference
                .target()
                .or_else(|| reference.peel_to_commit().ok().map(|commit| commit.id()))
                .map(|oid| oid.to_string());
            (reference.shorthand().unwrap_or("HEAD").to_string(), oid)
        }
        Err(_) => ("(no branch)".to_string(), None),
    };

    // Only fetch remote-tracking refs when explicitly requested
    let fetch_failed = if fetch {
        !fetch_remote_silent(path)
    } else {
        false
    };

    // Ahead/behind
    let (ahead, behind) = compute_ahead_behind(&repo);

    let ChangeSummary {
        files,
        is_dirty,
        has_submodules,
        submodules,
        has_dirty_submodules,
        has_unpushed_submodules,
    } = collect_change_summary(&repo, path, recurse_untracked_dirs, sub_cfg)?;

    // Collect linked worktree details (excludes the main working tree)
    let worktree_info = collect_worktree_info(&repo, sub_cfg);

    // Snapshot rendered ref tips (post-fetch, so moved remotes are reflected).
    let refs = graph_refs_fingerprint(&repo);

    Ok(RepoStatus {
        branch,
        head_oid,
        files,
        ahead,
        behind,
        is_dirty,
        worktree_info,
        has_submodules,
        submodules,
        has_dirty_submodules,
        has_unpushed_submodules,
        fetch_failed,
        stashes,
        refs,
    })
}

/// Resolve the repository's default branch as a diff-able ref name in
/// `<remote>/<branch>` form (so `git diff <base>...HEAD` works without a
/// matching local branch): the preferred remote's `HEAD` symbolic target
/// first, then its `main`, then its `master`. `None` when none resolve.
/// The remote comes from [`crate::git::preferred_remote`] so Gerrit/mirror
/// workspaces without an `origin` still get a review base; repos with
/// remote-tracking refs but no configured remotes fall back to `origin`.
pub(crate) fn default_branch_name(repo: &Repository) -> Option<String> {
    let remote = crate::git::preferred_remote(repo).unwrap_or_else(|| "origin".to_string());
    if let Ok(head_ref) = repo.find_reference(&format!("refs/remotes/{remote}/HEAD"))
        && let Ok(Some(target_name)) = head_ref.symbolic_target()
        && repo.find_reference(target_name).is_ok()
        && let Some(short) = target_name.strip_prefix("refs/remotes/")
    {
        return Some(short.to_string());
    }
    for branch in ["main", "master"] {
        if repo
            .find_reference(&format!("refs/remotes/{remote}/{branch}"))
            .is_ok()
        {
            return Some(format!("{remote}/{branch}"));
        }
    }
    None
}
