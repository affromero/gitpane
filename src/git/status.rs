use crate::config::SubmoduleConfig;
use git2::{Repository, StatusOptions, SubmoduleStatus};
use std::path::{Path, PathBuf};

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

struct ChangeSummary {
    files: Vec<FileEntry>,
    is_dirty: bool,
    has_submodules: bool,
    submodules: Vec<SubmoduleInfo>,
    has_dirty_submodules: bool,
    has_unpushed_submodules: bool,
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

impl SubmoduleWarn {
    pub fn is_clean(&self) -> bool {
        self.unpushed_commits == 0 && !self.pointer_unreachable && !self.needs_merge_to_default
    }
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

impl FileStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
            Self::Conflicted => "C",
        }
    }
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

fn collect_change_summary(
    repo: &Repository,
    path: &Path,
    recurse_untracked_dirs: bool,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<ChangeSummary> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(recurse_untracked_dirs)
        .renames_head_to_index(true);

    if sub_cfg.ignore_dirty {
        opts.exclude_submodules(true);
    }

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut files = Vec::new();

    for entry in statuses.iter() {
        let s = entry.status();
        let file_path = PathBuf::from(entry.path().unwrap_or(""));

        let file_status = if s.is_conflicted() {
            FileStatus::Conflicted
        } else if s.is_index_new() || s.is_wt_new() {
            if s.is_wt_new() && !s.is_index_new() {
                FileStatus::Untracked
            } else {
                FileStatus::Added
            }
        } else if s.is_index_deleted() || s.is_wt_deleted() {
            FileStatus::Deleted
        } else if s.is_index_renamed() || s.is_wt_renamed() {
            FileStatus::Renamed
        } else if s.is_index_modified() || s.is_wt_modified() {
            FileStatus::Modified
        } else {
            continue;
        };

        files.push(FileEntry {
            path: file_path,
            status: file_status,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        });
    }

    let is_dirty = !files.is_empty();

    // Detect submodules by checking for .gitmodules
    let has_submodules = path.join(".gitmodules").is_file();

    // Submodule enumeration
    let mut submodules = Vec::new();
    let mut has_dirty_submodules = false;
    let mut has_unpushed_submodules = false;

    // Iterate when *any* submodule signal is requested. `ignore_dirty` and
    // `warn_unpushed` are independent: even with dirty hidden, we may still
    // need to surface unpushed-pointer warnings.
    if has_submodules
        && (!sub_cfg.ignore_dirty || sub_cfg.warn_unpushed)
        && let Ok(subs) = repo.submodules()
    {
        for sub in &subs {
            let name = sub.name().unwrap_or("").to_string();
            let sub_path = PathBuf::from(sub.path());

            // Dirty-state mapping (gated on !ignore_dirty).
            let state = if sub_cfg.ignore_dirty {
                None
            } else {
                let status = repo
                    .submodule_status(&name, git2::SubmoduleIgnore::Unspecified)
                    .unwrap_or(SubmoduleStatus::empty());
                if status.is_wd_uninitialized() {
                    Some(SubmoduleState::Uninitialized)
                } else if status.is_wd_wd_modified()
                    || status.contains(SubmoduleStatus::WD_UNTRACKED)
                {
                    Some(SubmoduleState::Dirty)
                } else if status.is_wd_modified()
                    || status.contains(SubmoduleStatus::WD_INDEX_MODIFIED)
                {
                    Some(SubmoduleState::Modified)
                } else {
                    None
                }
            };

            // Open the submodule (once) to read its checked-out branch and,
            // when enabled, compute push/merge warnings. Skip for uninitialized
            // submodules (`sub.open()` fails) and when there is nothing to show
            // (clean working tree with warnings disabled): branch is only worth
            // reading for a submodule that will render a row.
            let is_uninit = state == Some(SubmoduleState::Uninitialized);
            let (head, warn) = if !is_uninit && (state.is_some() || sub_cfg.warn_unpushed) {
                compute_submodule_head_and_warn(sub, sub_cfg.warn_unpushed)
            } else {
                (None, SubmoduleWarn::default())
            };

            let has_dirty_signal = state.is_some();
            let has_warn_signal = !warn.is_clean();

            if !has_dirty_signal && !has_warn_signal {
                continue;
            }

            let head_oid = sub.head_id().map(|id| id.to_string());
            let workdir_oid = sub.workdir_id().map(|id| id.to_string());

            submodules.push(SubmoduleInfo {
                name: name.clone(),
                path: sub_path.clone(),
                state: state.clone(),
                head: head.clone(),
                head_oid,
                workdir_oid,
                warn,
            });

            // Cross-reference with files vec
            if let Some(file_entry) = files.iter_mut().find(|f| f.path == sub_path) {
                file_entry.is_submodule = true;
                file_entry.submodule_state = state.clone();
                file_entry.submodule_warn = warn;
                file_entry.submodule_head = head;
            } else {
                // Synthetic FileEntry for any submodule with a dirty or warn signal.
                // FileStatus::Modified keeps the leading `M` "needs attention" cue;
                // the [sub: ...] tag carries the actual semantics.
                files.push(FileEntry {
                    path: sub_path,
                    status: FileStatus::Modified,
                    is_submodule: true,
                    submodule_state: state,
                    submodule_warn: warn,
                    submodule_head: head,
                });
            }

            if has_dirty_signal {
                has_dirty_submodules = true;
            }
            if has_warn_signal {
                has_unpushed_submodules = true;
            }
        }
    }

    Ok(ChangeSummary {
        files,
        is_dirty: is_dirty || has_dirty_submodules,
        has_submodules,
        submodules,
        has_dirty_submodules,
        has_unpushed_submodules,
    })
}

/// Open the submodule once to read its checked-out branch and, when
/// `warn_enabled`, compute its push/merge warnings. Returns `(head, warn)`;
/// `head` is `None` only when the submodule cannot be opened.
fn compute_submodule_head_and_warn(
    sub: &git2::Submodule,
    warn_enabled: bool,
) -> (Option<SubmoduleHead>, SubmoduleWarn) {
    let inner = match sub.open() {
        Ok(r) => r,
        Err(_) => return (None, SubmoduleWarn::default()),
    };

    let head = Some(read_submodule_head(&inner));
    let warn = if warn_enabled {
        compute_submodule_warn(sub, &inner)
    } else {
        SubmoduleWarn::default()
    };
    (head, warn)
}

/// Read the submodule's current branch, or `Detached` when it sits on a
/// detached HEAD (git's default after `submodule update`) or has no branch.
fn read_submodule_head(inner: &Repository) -> SubmoduleHead {
    if inner.head_detached().unwrap_or(true) {
        return SubmoduleHead::Detached;
    }
    match inner.head() {
        Ok(head) => SubmoduleHead::Branch(head.shorthand().unwrap_or("HEAD").to_string()),
        Err(_) => SubmoduleHead::Detached,
    }
}

/// Compute "you owe a push/merge" warnings for a submodule, given an already
/// open handle to it. Uses `index_id()` (parent's *staged* pointer, not
/// committed) so the warning fires *before* the parent commit ships, while the
/// user can still amend or reset.
fn compute_submodule_warn(sub: &git2::Submodule, inner: &Repository) -> SubmoduleWarn {
    let recorded = match sub.index_id() {
        Some(o) => o,
        None => return SubmoduleWarn::default(),
    };

    let unpushed_commits = compute_ahead_behind(inner).0;

    // If the recorded oid isn't even in local objects, no remote can possibly hold it.
    if inner.find_object(recorded, None).is_err() {
        return SubmoduleWarn {
            unpushed_commits,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
    }

    if !remote_reaches(inner, recorded) {
        // No remote tip reaches `recorded` (or no remotes configured) → must push.
        return SubmoduleWarn {
            unpushed_commits,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
    }

    // Reachable from some remote. If we can resolve the default branch and it
    // does NOT reach `recorded`, the commit lives on a side branch and still
    // needs a merge there. When no default branch resolves, stay silent.
    let needs_merge_to_default = match default_branch_tip(inner) {
        Some(tip) => !reaches(inner, tip, recorded),
        None => false,
    };

    SubmoduleWarn {
        unpushed_commits,
        pointer_unreachable: false,
        needs_merge_to_default,
    }
}

/// Whether `recorded` is reachable from `tip` (equal, or an ancestor of it).
fn reaches(inner: &Repository, tip: git2::Oid, recorded: git2::Oid) -> bool {
    tip == recorded || inner.graph_descendant_of(tip, recorded).unwrap_or(false)
}

/// Whether `recorded` is reachable from any `refs/remotes/*` branch tip.
fn remote_reaches(inner: &Repository, recorded: git2::Oid) -> bool {
    let Ok(branches) = inner.branches(Some(git2::BranchType::Remote)) else {
        return false;
    };
    for (b, _) in branches.flatten() {
        if let Some(tip) = b.get().target()
            && reaches(inner, tip, recorded)
        {
            return true;
        }
    }
    false
}

/// Resolve the submodule's default-branch tip: `origin/HEAD`'s symbolic target
/// first, then `origin/main`, then `origin/master`. `None` when none resolve —
/// common in submodule clones, where `origin/HEAD` is often absent.
fn default_branch_tip(inner: &Repository) -> Option<git2::Oid> {
    if let Ok(head_ref) = inner.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target_name)) = head_ref.symbolic_target()
        && let Ok(target_ref) = inner.find_reference(target_name)
        && let Some(oid) = target_ref.target()
    {
        return Some(oid);
    }
    for name in ["refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if let Ok(r) = inner.find_reference(name)
            && let Some(oid) = r.target()
        {
            return Some(oid);
        }
    }
    None
}

/// Resolve the repository's default branch as a diff-able ref name in
/// `origin/<branch>` form (so `git diff <base>...HEAD` works without a matching
/// local branch): `origin/HEAD`'s symbolic target first, then `origin/main`,
/// then `origin/master`. `None` when none resolve.
pub(crate) fn default_branch_name(repo: &Repository) -> Option<String> {
    if let Ok(head_ref) = repo.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target_name)) = head_ref.symbolic_target()
        && repo.find_reference(target_name).is_ok()
        && let Some(short) = target_name.strip_prefix("refs/remotes/")
    {
        return Some(short.to_string());
    }
    for (full, short) in [
        ("refs/remotes/origin/main", "origin/main"),
        ("refs/remotes/origin/master", "origin/master"),
    ] {
        if repo.find_reference(full).is_ok() {
            return Some(short.to_string());
        }
    }
    None
}

/// Collect details for each linked worktree using the git2 API.
/// Mirrors the pattern in `git/graph.rs::collect_worktree_branches`.
fn collect_worktree_info(repo: &Repository, sub_cfg: &SubmoduleConfig) -> Vec<WorktreeEntry> {
    let wt_names = match repo.worktrees() {
        Ok(names) => names,
        Err(_) => return Vec::new(),
    };
    let mut entries = Vec::new();
    for i in 0..wt_names.len() {
        let name = match wt_names.get(i) {
            Ok(Some(n)) => n,
            _ => continue,
        };
        let wt = match repo.find_worktree(name) {
            Ok(wt) => wt,
            Err(_) => continue,
        };
        let wt_path = wt.path().to_path_buf();
        let wt_repo = match Repository::open(&wt_path) {
            Ok(wt_repo) => wt_repo,
            Err(_) => continue,
        };
        let branch = match wt_repo.head() {
            Ok(head) => head.shorthand().unwrap_or("HEAD").to_string(),
            Err(_) => "(no branch)".to_string(),
        };
        let (ahead, behind) = compute_ahead_behind(&wt_repo);
        let summary =
            collect_change_summary(&wt_repo, &wt_path, false, sub_cfg).unwrap_or(ChangeSummary {
                files: Vec::new(),
                is_dirty: false,
                has_submodules: false,
                submodules: Vec::new(),
                has_dirty_submodules: false,
                has_unpushed_submodules: false,
            });

        entries.push(WorktreeEntry {
            name: name.to_string(),
            path: wt_path,
            branch,
            ahead,
            behind,
            is_dirty: summary.is_dirty,
            file_count: summary.files.len(),
            has_dirty_submodules: summary.has_dirty_submodules,
            has_unpushed_submodules: summary.has_unpushed_submodules,
        });
    }
    entries
}

/// Run `git fetch` with a 30-second timeout to update remote-tracking refs.
/// Uses the CLI because git2 fetch doesn't support SSH agent / credential helpers
/// out of the box. Returns `true` on success, `false` on failure/timeout.
fn fetch_remote_silent(path: &Path) -> bool {
    use wait_timeout::ChildExt;

    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("fetch")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match child {
        Ok(mut c) => {
            match c.wait_timeout(std::time::Duration::from_secs(30)) {
                Ok(Some(status)) => status.success(),
                Ok(None) => {
                    // Timed out — kill the hung process
                    let _ = c.kill();
                    let _ = c.wait();
                    false
                }
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// Returns (ahead, behind) for HEAD relative to its publishing target.
///
/// Fast path: if HEAD's branch has a configured upstream, use git2's
/// `graph_ahead_behind` against that upstream's tip.
///
/// Fallback (no upstream, e.g. a freshly-created local branch): walk HEAD
/// hiding every `refs/remotes/*` tip, count what remains. Semantics match
/// `git log HEAD --not --remotes`. `behind` is always 0 in this case (there
/// is no single ref to be behind of). If the repo has no remote-tracking
/// refs at all, returns (0, 0) since "unpushed" needs a remote to mean
/// anything.
fn compute_ahead_behind(repo: &Repository) -> (usize, usize) {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return (0, 0),
    };

    let local_oid = match head.target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    let branch_name = match head.shorthand() {
        Ok(name) => name.to_string(),
        Err(_) => return (0, 0),
    };

    let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return (0, 0),
    };

    if let Ok(upstream) = branch.upstream()
        && let Some(upstream_oid) = upstream.get().target()
    {
        return repo
            .graph_ahead_behind(local_oid, upstream_oid)
            .unwrap_or((0, 0));
    }

    ahead_against_remote_tips(repo, local_oid)
}

/// Count commits reachable from `local_oid` but not from any
/// `refs/remotes/*` tip. Used when the branch has no configured upstream.
fn ahead_against_remote_tips(repo: &Repository, local_oid: git2::Oid) -> (usize, usize) {
    let mut walk = match repo.revwalk() {
        Ok(w) => w,
        Err(_) => return (0, 0),
    };
    if walk.push(local_oid).is_err() {
        return (0, 0);
    }

    let mut any_remote = false;
    if let Ok(branches) = repo.branches(Some(git2::BranchType::Remote)) {
        for (branch, _) in branches.flatten() {
            if let Some(tip) = branch.get().target() {
                any_remote = true;
                let _ = walk.hide(tip);
            }
        }
    }

    if !any_remote {
        return (0, 0);
    }

    let count = walk.filter_map(Result::ok).count();
    (count, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn commit_index(repo: &Repository, message: &str) -> git2::Oid {
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = head.iter().collect();

        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    fn init_temp_repo() -> (TempDir, Repository) {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Create initial commit so HEAD exists
        commit_index(&repo, "Initial commit");

        (tmp, repo)
    }

    #[test]
    fn test_clean_repo_reports_no_changes() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(!status.is_dirty);
        assert!(status.files.is_empty());
    }

    #[test]
    fn test_modified_file_detected() {
        let (tmp, repo) = init_temp_repo();

        // Add and commit a file
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("test.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "Add file", &tree, &[&head])
            .unwrap();

        // Modify it
        fs::write(&file_path, "world").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.status == FileStatus::Modified)
        );
    }

    #[test]
    fn test_untracked_file_detected() {
        let (tmp, _repo) = init_temp_repo();
        fs::write(tmp.path().join("new.txt"), "new").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.status == FileStatus::Untracked)
        );
    }

    #[test]
    fn test_background_status_does_not_recurse_untracked_dirs() {
        let (tmp, _repo) = init_temp_repo();
        fs::create_dir_all(tmp.path().join("nested")).unwrap();
        fs::write(tmp.path().join("nested/file.txt"), "new").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.status == FileStatus::Untracked)
        );
        assert!(
            !status
                .files
                .iter()
                .any(|f| f.path == Path::new("nested/file.txt"))
        );

        let recursive =
            query_status_inner(tmp.path(), false, true, &SubmoduleConfig::default()).unwrap();
        assert!(
            recursive
                .files
                .iter()
                .any(|f| f.path == Path::new("nested/file.txt"))
        );
    }

    #[test]
    fn test_default_branch_name_resolves_origin_main() {
        let (_tmp, repo) = init_temp_repo();
        let oid = repo.head().unwrap().target().unwrap();
        repo.reference("refs/remotes/origin/main", oid, true, "test")
            .unwrap();
        assert_eq!(default_branch_name(&repo).as_deref(), Some("origin/main"));
    }

    #[test]
    fn test_default_branch_name_none_without_remote() {
        let (_tmp, repo) = init_temp_repo();
        assert_eq!(default_branch_name(&repo), None);
    }

    #[test]
    fn test_worktree_info_empty_for_plain_repo() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.worktree_info.is_empty());
    }

    #[test]
    fn test_worktree_info_reflects_linked_worktrees() {
        let (tmp, repo) = init_temp_repo();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch("wt-branch", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));

        let wt_tmp = TempDir::new().unwrap();
        let wt_dir = wt_tmp.path().join("wt1");
        repo.worktree("wt1", &wt_dir, Some(&opts)).unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.worktree_info.len(), 1);
        assert_eq!(status.worktree_info[0].branch, "wt-branch");
        assert_eq!(status.worktree_info[0].name, "wt1");
        assert!(!status.worktree_info[0].is_dirty);
        assert_eq!(status.worktree_info[0].file_count, 0);
        assert!(!status.worktree_info[0].has_dirty_submodules);
        assert!(!status.worktree_info[0].has_unpushed_submodules);
    }

    #[test]
    fn test_worktree_info_reflects_dirty_linked_worktree() {
        let (tmp, repo) = init_temp_repo();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch("wt-dirty", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));

        let wt_tmp = TempDir::new().unwrap();
        let wt_dir = wt_tmp.path().join("wt-dirty");
        repo.worktree("wt-dirty", &wt_dir, Some(&opts)).unwrap();
        fs::write(wt_dir.join("new.txt"), "new").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        let wt = status
            .worktree_info
            .iter()
            .find(|wt| wt.name == "wt-dirty")
            .unwrap();

        assert!(wt.is_dirty);
        assert_eq!(wt.file_count, 1);
        assert!(!wt.has_dirty_submodules);
        assert!(!wt.has_unpushed_submodules);
    }

    #[test]
    fn test_worktree_info_reflects_submodule_signals() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        let repo = Repository::open(tmp.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch("wt-submodule", &head, false).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));

        let wt_tmp = TempDir::new().unwrap();
        let wt_dir = wt_tmp.path().join("wt-submodule");
        repo.worktree("wt-submodule", &wt_dir, Some(&opts)).unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        let wt = status
            .worktree_info
            .iter()
            .find(|wt| wt.name == "wt-submodule")
            .unwrap();

        assert!(wt.is_dirty);
        assert!(wt.file_count > 0);
        assert!(wt.has_dirty_submodules);
    }

    #[test]
    fn test_submodule_state_mapping() {
        // Test the flag-to-state conversion logic
        let flags = SubmoduleStatus::WD_UNINITIALIZED;
        assert!(flags.is_wd_uninitialized());

        let flags = SubmoduleStatus::WD_WD_MODIFIED;
        assert!(flags.is_wd_wd_modified());

        let flags = SubmoduleStatus::WD_UNTRACKED;
        assert!(flags.contains(SubmoduleStatus::WD_UNTRACKED));

        let flags = SubmoduleStatus::WD_MODIFIED;
        assert!(flags.is_wd_modified());

        let flags = SubmoduleStatus::WD_INDEX_MODIFIED;
        assert!(flags.contains(SubmoduleStatus::WD_INDEX_MODIFIED));
    }

    #[test]
    fn test_clean_repo_no_dirty_submodules() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
    }

    #[test]
    fn test_status_maps_correctly() {
        assert_eq!(FileStatus::Modified.label(), "M");
        assert_eq!(FileStatus::Added.label(), "A");
        assert_eq!(FileStatus::Deleted.label(), "D");
        assert_eq!(FileStatus::Renamed.label(), "R");
        assert_eq!(FileStatus::Untracked.label(), "?");
        assert_eq!(FileStatus::Conflicted.label(), "C");
    }

    #[test]
    fn test_file_entry_submodule_fields() {
        let entry = FileEntry {
            path: PathBuf::from("my-submodule"),
            status: FileStatus::Modified,
            is_submodule: true,
            submodule_state: Some(SubmoduleState::Modified),
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: Some(SubmoduleHead::Branch("feature".to_string())),
        };
        assert!(entry.is_submodule);
        assert_eq!(entry.submodule_state, Some(SubmoduleState::Modified));
        assert!(entry.submodule_warn.is_clean());
        assert_eq!(
            entry.submodule_head,
            Some(SubmoduleHead::Branch("feature".to_string()))
        );

        let plain = FileEntry {
            path: PathBuf::from("src/main.rs"),
            status: FileStatus::Modified,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        };
        assert!(!plain.is_submodule);
        assert_eq!(plain.submodule_state, None);
        assert_eq!(plain.submodule_head, None);
    }

    #[test]
    fn test_submodule_state_equality_and_clone() {
        assert_eq!(SubmoduleState::Modified, SubmoduleState::Modified);
        assert_eq!(SubmoduleState::Dirty, SubmoduleState::Dirty);
        assert_eq!(SubmoduleState::Uninitialized, SubmoduleState::Uninitialized);
        assert_ne!(SubmoduleState::Modified, SubmoduleState::Dirty);
        assert_ne!(SubmoduleState::Dirty, SubmoduleState::Uninitialized);

        let state = SubmoduleState::Modified;
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_ignore_dirty_subs_on_clean_repo() {
        // ignore_dirty_subs = true should work fine on repos without submodules
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(
            tmp.path(),
            &SubmoduleConfig {
                ignore_dirty: true,
                warn_unpushed: false,
            },
        )
        .unwrap();
        assert!(!status.is_dirty);
        assert!(status.files.is_empty());
        assert!(status.submodules.is_empty());
        assert!(!status.has_dirty_submodules);
    }

    #[test]
    fn test_ignore_dirty_subs_still_detects_regular_changes() {
        let (tmp, _repo) = init_temp_repo();
        fs::write(tmp.path().join("new.txt"), "new").unwrap();

        let status = query_status(
            tmp.path(),
            &SubmoduleConfig {
                ignore_dirty: true,
                warn_unpushed: false,
            },
        )
        .unwrap();
        assert!(status.is_dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.status == FileStatus::Untracked)
        );
        // Submodule fields should be empty when ignored
        assert!(status.submodules.is_empty());
        assert!(!status.has_dirty_submodules);
    }

    #[test]
    fn test_submodule_state_priority_uninitialized_first() {
        // WD_UNINITIALIZED takes priority over other flags
        let flags = SubmoduleStatus::WD_UNINITIALIZED | SubmoduleStatus::WD_MODIFIED;
        assert!(flags.is_wd_uninitialized());
    }

    #[test]
    fn test_submodule_state_priority_dirty_over_modified() {
        // WD_WD_MODIFIED (dirty) is checked before WD_MODIFIED (pointer change)
        let flags = SubmoduleStatus::WD_WD_MODIFIED | SubmoduleStatus::WD_MODIFIED;
        assert!(flags.is_wd_wd_modified());
        assert!(flags.is_wd_modified());
        // Our mapping logic checks dirty first, so this should map to Dirty
    }

    /// Helper: creates a temp repo with a submodule, returns (parent_tmp, sub_source_tmp, sub_repo)
    fn init_repo_with_submodule() -> (TempDir, TempDir, Repository) {
        let (tmp, repo) = init_temp_repo();

        let sub_source = TempDir::new().unwrap();
        let sub_repo = Repository::init(sub_source.path()).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            fs::write(sub_source.path().join("lib.rs"), "fn hello() {}").unwrap();
            let mut idx = sub_repo.index().unwrap();
            idx.add_path(Path::new("lib.rs")).unwrap();
            idx.write().unwrap();
            let tree_id = idx.write_tree().unwrap();
            let tree = sub_repo.find_tree(tree_id).unwrap();
            sub_repo
                .commit(Some("HEAD"), &sig, &sig, "init sub", &tree, &[])
                .unwrap();
        }

        let mut submodule = repo
            .submodule(
                sub_source.path().to_str().unwrap(),
                Path::new("my-sub"),
                true,
            )
            .unwrap();
        submodule.clone(None).unwrap();
        submodule.add_finalize().unwrap();
        commit_index(&repo, "add submodule");

        (tmp, sub_source, sub_repo)
    }

    fn stage_submodule_pointer(parent_path: &Path, sub_dirname: &str) {
        let repo = Repository::open(parent_path).unwrap();
        let mut submodule = repo.find_submodule(sub_dirname).unwrap();
        submodule.reload(true).unwrap();
        submodule.add_to_index(true).unwrap();
    }

    #[test]
    fn test_dirty_submodule_with_real_git_submodule() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Verify: clean state should show has_submodules but no dirty submodules
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());

        // Now make the submodule dirty by modifying a file inside it
        let sub_workdir = tmp.path().join("my-sub");
        fs::write(sub_workdir.join("lib.rs"), "fn hello() { /* changed */ }").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        assert_eq!(sub_info.state, Some(SubmoduleState::Dirty));

        // Verify the file entry is annotated
        let file_entry = status.files.iter().find(|f| f.path == Path::new("my-sub"));
        assert!(file_entry.is_some());
        let file_entry = file_entry.unwrap();
        assert!(file_entry.is_submodule);
        assert_eq!(file_entry.submodule_state, Some(SubmoduleState::Dirty));
    }

    #[test]
    fn test_ignore_dirty_subs_hides_submodule_state() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Make the submodule dirty
        fs::write(tmp.path().join("my-sub/lib.rs"), "fn changed() {}").unwrap();

        // With ignore_dirty_subs = true, submodule state should be hidden
        let status = query_status(
            tmp.path(),
            &SubmoduleConfig {
                ignore_dirty: true,
                warn_unpushed: false,
            },
        )
        .unwrap();
        assert!(status.has_submodules); // .gitmodules still exists
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
        // No submodule-annotated entries
        assert!(!status.files.iter().any(|f| f.is_submodule));
    }

    #[test]
    fn test_submodule_modified_pointer() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        add_unpushed_commit_in_sub(tmp.path(), "my-sub");

        // Now the submodule pointer has changed (HEAD in submodule != recorded in parent)
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        // Could be Modified or Dirty depending on exact git state
        assert!(
            sub_info.state == Some(SubmoduleState::Modified)
                || sub_info.state == Some(SubmoduleState::Dirty),
            "expected Modified or Dirty, got {:?}",
            sub_info.state
        );

        // Verify OIDs are populated
        assert!(sub_info.head_oid.is_some());
        assert!(sub_info.workdir_oid.is_some());
        assert_ne!(sub_info.head_oid, sub_info.workdir_oid);
    }

    #[test]
    fn test_clean_submodule_not_reported() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Without any modifications, the submodule should be clean
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
        assert!(!status.files.iter().any(|f| f.is_submodule));
    }

    #[test]
    fn test_dirty_submodule_makes_repo_dirty() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Start clean
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(!status.is_dirty);

        // Make submodule dirty
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        // Now repo should be dirty
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
    }

    #[test]
    fn test_submodule_warn_default_is_clean() {
        let warn = SubmoduleWarn::default();
        assert!(warn.is_clean());
        assert_eq!(warn.unpushed_commits, 0);
        assert!(!warn.pointer_unreachable);
    }

    #[test]
    fn test_submodule_warn_is_clean_predicate() {
        assert!(SubmoduleWarn::default().is_clean());
        assert!(
            !SubmoduleWarn {
                unpushed_commits: 1,
                pointer_unreachable: false,
                needs_merge_to_default: false,
            }
            .is_clean()
        );
        assert!(
            !SubmoduleWarn {
                unpushed_commits: 0,
                pointer_unreachable: true,
                needs_merge_to_default: false,
            }
            .is_clean()
        );
        assert!(
            !SubmoduleWarn {
                unpushed_commits: 0,
                pointer_unreachable: false,
                needs_merge_to_default: true,
            }
            .is_clean()
        );
    }

    #[test]
    fn test_clean_submodule_warn_fields_clean() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        // Freshly cloned via `git submodule add` — recorded oid is on origin.
        assert!(!status.has_unpushed_submodules);
    }

    #[test]
    fn test_dirty_submodule_warn_fields_stay_clean() {
        // A workdir-dirty submodule should not trigger warn fields — only
        // pointer changes / unpushed commits matter for "you owe a push".
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        let sub_info = &status.submodules[0];
        assert!(sub_info.warn.is_clean());

        let file_entry = status
            .files
            .iter()
            .find(|f| f.path == Path::new("my-sub"))
            .unwrap();
        assert!(file_entry.submodule_warn.is_clean());
    }

    #[test]
    fn test_warn_unpushed_false_zeros_warn_fields_even_when_dirty() {
        // With warn_unpushed=false, dirty subs still surface but warn fields stay zero.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: false,
            warn_unpushed: false,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        assert!(status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        assert!(status.submodules[0].warn.is_clean());
    }

    #[test]
    fn test_ignore_dirty_with_warn_unpushed_iterates_subs() {
        // When ignore_dirty=true but warn_unpushed=true, the loop must still
        // iterate submodules to compute warn fields. Dirty state itself is hidden.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: true,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        // Dirty signal hidden
        assert!(!status.has_dirty_submodules);
        // Warn signal also clean for this freshly-added submodule
        assert!(!status.has_unpushed_submodules);
        // No submodule entries (nothing to warn about; dirty hidden)
        assert!(status.submodules.is_empty());
    }

    #[test]
    fn test_both_flags_off_skips_submodule_loop_entirely() {
        // When both flags are off, the loop body is skipped — fast path.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: false,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        assert!(status.has_submodules); // .gitmodules still exists
        assert!(!status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        assert!(status.submodules.is_empty());
    }

    #[test]
    fn test_uninitialized_submodule_does_not_panic_on_warn_check() {
        // De-init the submodule so .git is absent — `sub.open()` should fail
        // gracefully and `compute_submodule_warn` returns default.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::remove_dir_all(tmp.path().join("my-sub")).unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        // No panic. The submodule may or may not appear in `submodules` (the
        // dirty-state check still classifies it as Uninitialized) — the key
        // invariant is that warn fields stay clean.
        for sub in &status.submodules {
            assert!(sub.warn.is_clean());
        }
        assert!(!status.has_unpushed_submodules);
    }

    // ---- Integration tests with a real submodule + remote-tracking ref ----

    /// Adds a new commit on top of the submodule's current HEAD without pushing.
    /// Updates HEAD (works whether HEAD is detached or on a branch).
    /// Returns the new commit oid.
    fn add_unpushed_commit_in_sub(parent_path: &Path, sub_dirname: &str) -> git2::Oid {
        let sub_repo = Repository::open(parent_path.join(sub_dirname)).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let head_commit = sub_repo.head().unwrap().peel_to_commit().unwrap();

        fs::write(
            parent_path.join(sub_dirname).join("extra.rs"),
            "fn extra() {}",
        )
        .unwrap();
        let mut idx = sub_repo.index().unwrap();
        idx.add_path(Path::new("extra.rs")).unwrap();
        idx.write().unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = sub_repo.find_tree(tree_id).unwrap();

        sub_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "unpushed work",
                &tree,
                &[&head_commit],
            )
            .unwrap()
    }

    #[test]
    fn test_parent_pinning_unpushed_oid_marks_pointer_unreachable() {
        // Reproduce the footgun: commit in submodule (not on remote), then
        // `git add my-sub` in parent — staging an oid that no remote can resolve.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        let unpushed = add_unpushed_commit_in_sub(tmp.path(), "my-sub");

        // Stage the new submodule pointer in the parent's index.
        stage_submodule_pointer(tmp.path(), "my-sub");

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_unpushed_submodules);

        let sub_info = status
            .submodules
            .iter()
            .find(|s| s.path == Path::new("my-sub"))
            .expect("submodule entry should exist with warn signal");
        assert!(
            sub_info.warn.pointer_unreachable,
            "expected pointer_unreachable=true for staged oid {}, got {:?}",
            unpushed, sub_info.warn
        );

        // The file row should also surface the warn fields.
        let file_entry = status
            .files
            .iter()
            .find(|f| f.path == Path::new("my-sub"))
            .expect("file entry for my-sub");
        assert!(file_entry.is_submodule);
        assert!(file_entry.submodule_warn.pointer_unreachable);
    }

    #[test]
    fn test_warn_unpushed_false_zeros_unreachable_pointer() {
        // Same setup as the previous test, but with warn_unpushed=false the
        // warn fields must stay zero even though the pointer is unreachable.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        add_unpushed_commit_in_sub(tmp.path(), "my-sub");
        stage_submodule_pointer(tmp.path(), "my-sub");

        let cfg = SubmoduleConfig {
            ignore_dirty: false,
            warn_unpushed: false,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        assert!(!status.has_unpushed_submodules);
        for sub in &status.submodules {
            assert!(
                sub.warn.is_clean(),
                "warn fields must be zero when warn_unpushed=false, got {:?}",
                sub.warn
            );
        }
    }

    #[test]
    fn test_unpushed_commits_count_when_branch_ahead_of_upstream() {
        // The submodule's local branch advances past its upstream remote ref;
        // the parent's recorded oid is unchanged (still reachable on origin),
        // so unpushed_commits>0 with pointer_unreachable=false.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Find the cloned submodule's default remote branch name. After
        // `git submodule add`, the inner repo has a `refs/remotes/origin/<branch>`
        // ref. Use it to set up a local tracking branch.
        let sub_dir = tmp.path().join("my-sub");
        let inner = Repository::open(&sub_dir).unwrap();
        let mut remote_branch: Option<String> = None;
        if let Ok(branches) = inner.branches(Some(git2::BranchType::Remote)) {
            for (b, _) in branches.flatten() {
                if let Ok(Some(name)) = b.name()
                    && let Some(stripped) = name.strip_prefix("origin/")
                    && stripped != "HEAD"
                {
                    remote_branch = Some(stripped.to_string());
                    break;
                }
            }
        }
        let branch = remote_branch.expect("submodule should have an origin/<branch> ref");

        // Create a local tracking branch pointing at HEAD and set its upstream.
        let remote_ref = inner
            .find_reference(&format!("refs/remotes/origin/{}", branch))
            .unwrap();
        let remote_oid = remote_ref.target().unwrap();
        let remote_commit = inner.find_commit(remote_oid).unwrap();
        let mut local_branch = inner
            .find_branch(&branch, git2::BranchType::Local)
            .or_else(|_| inner.branch(&branch, &remote_commit, false))
            .unwrap();
        local_branch
            .set_upstream(Some(&format!("origin/{}", branch)))
            .unwrap();
        inner.set_head(&format!("refs/heads/{}", branch)).unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force();
        inner.checkout_head(Some(&mut checkout)).unwrap();

        // Add an unpushed commit (local branch advances past origin/<branch>).
        add_unpushed_commit_in_sub(tmp.path(), "my-sub");

        // The parent has not staged a new pointer, so its recorded oid is
        // still the prior commit — present on origin.
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_unpushed_submodules);

        let sub_info = status
            .submodules
            .iter()
            .find(|s| s.path == Path::new("my-sub"))
            .expect("submodule entry expected");
        assert_eq!(
            sub_info.warn.unpushed_commits, 1,
            "expected 1 unpushed commit, got {:?}",
            sub_info.warn
        );
        assert!(
            !sub_info.warn.pointer_unreachable,
            "parent's pointer is unchanged and on origin — should be reachable"
        );
    }

    #[test]
    fn test_detached_head_at_remote_oid_no_warn() {
        // A submodule with detached HEAD at an oid present on a remote ref
        // should produce no warn signal.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // After `git submodule add`, HEAD is typically already detached at the
        // initial cloned commit, which is on `refs/remotes/origin/<branch>`.
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(!status.has_unpushed_submodules);
        for sub in &status.submodules {
            assert!(sub.warn.is_clean());
        }
    }

    #[test]
    fn test_needs_merge_to_default_when_pinned_on_side_branch() {
        // The parent pins a submodule commit that lives on a remote *side*
        // branch (origin/feature) but is not reachable from the default branch
        // (origin/main). The commit is fetchable, so it is not "unreachable",
        // yet it still needs merging to main → `needs_merge_to_default`.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        let sub_dir = tmp.path().join("my-sub");
        let inner = Repository::open(&sub_dir).unwrap();

        // A: the initial cloned commit. Anchor the default branch here.
        let a = inner.head().unwrap().peel_to_commit().unwrap().id();
        inner
            .reference("refs/remotes/origin/main", a, true, "test setup")
            .unwrap();

        // B: a new commit (child of A) now checked out in the submodule,
        // published only on origin/feature.
        let b = add_commit_on_head(&inner, "feature.rs", "fn f() {}");
        inner
            .reference("refs/remotes/origin/feature", b, true, "test setup")
            .unwrap();

        // Stage the parent's pointer at B (the submodule's current HEAD).
        stage_submodule_pointer(tmp.path(), "my-sub");

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        let sub = status
            .submodules
            .iter()
            .find(|s| s.path == Path::new("my-sub"))
            .expect("submodule entry expected");

        assert!(
            sub.warn.needs_merge_to_default,
            "pinned commit is on origin/feature, not origin/main: {:?}",
            sub.warn
        );
        assert!(
            !sub.warn.pointer_unreachable,
            "commit is on a remote, so it is reachable: {:?}",
            sub.warn
        );
        assert!(!sub.warn.is_clean());
        assert!(status.has_unpushed_submodules);
    }

    /// A pinned commit that IS on the default branch must not warn.
    #[test]
    fn test_no_merge_warning_when_pinned_on_default_branch() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        let sub_dir = tmp.path().join("my-sub");
        let inner = Repository::open(&sub_dir).unwrap();
        let a = inner.head().unwrap().peel_to_commit().unwrap().id();
        // Default branch points at the very commit the parent pins.
        inner
            .reference("refs/remotes/origin/main", a, true, "test setup")
            .unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        for sub in &status.submodules {
            assert!(
                !sub.warn.needs_merge_to_default,
                "pinned commit is on origin/main: {:?}",
                sub.warn
            );
        }
    }

    fn add_commit_on_head(repo: &Repository, file: &str, contents: &str) -> git2::Oid {
        let workdir = repo.workdir().unwrap().to_path_buf();
        fs::write(workdir.join(file), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("Add {}", file),
            &tree,
            &[&parent],
        )
        .unwrap()
    }

    #[test]
    fn test_ahead_count_when_no_upstream_with_remote_ref() {
        // Local branch with no configured upstream but with a remote-tracking
        // ref at commit A. After two new local commits B, C, ahead == 2.
        let (tmp, repo) = init_temp_repo();
        let a = repo.head().unwrap().target().unwrap();
        repo.reference("refs/remotes/origin/main", a, true, "test setup")
            .unwrap();
        add_commit_on_head(&repo, "b.txt", "b");
        add_commit_on_head(&repo, "c.txt", "c");

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn test_no_remotes_keeps_zero_ahead() {
        // Repo with no remote-tracking refs at all. Two commits beyond the
        // initial one should report ahead == 0 because "unpushed" needs a
        // remote to mean anything.
        let (tmp, repo) = init_temp_repo();
        add_commit_on_head(&repo, "b.txt", "b");
        add_commit_on_head(&repo, "c.txt", "c");

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn test_stash_count_zero_when_no_stash() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.stashes.is_empty());
        assert_eq!(status.stash_count(), 0);
    }

    #[test]
    fn test_stash_count_reflects_stash_save() {
        let (tmp, mut repo) = init_temp_repo();
        // Add a tracked file in an initial commit so stash has a baseline.
        add_commit_on_head(&repo, "tracked.txt", "v1");

        // Modify it and stash.
        fs::write(tmp.path().join("tracked.txt"), "v2").unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        repo.stash_save2(&sig, None, None).unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.stash_count(), 1);
        assert!(!status.stashes[0].oid.is_empty());

        // Stash again.
        fs::write(tmp.path().join("tracked.txt"), "v3").unwrap();
        repo.stash_save2(&sig, None, None).unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.stash_count(), 2);
        // Indices increase with insertion order so older stash is at higher index in libgit2.
        assert!(status.stashes.iter().any(|s| s.index == 0));
        assert!(status.stashes.iter().any(|s| s.index == 1));
    }

    #[test]
    fn test_ahead_count_when_head_shares_only_root_with_unrelated_remote() {
        // HEAD chain: A -> B -> C. Remote tip: D (a sibling of B, off A).
        // The merge-base of HEAD and the remote tip is A. Walking C and
        // hiding D hides D and A; C and B remain. Expect ahead == 2.
        let (tmp, repo) = init_temp_repo();
        let a = repo.head().unwrap().target().unwrap();

        // Build an unrelated sibling commit D off A on a detached state.
        let a_commit = repo.find_commit(a).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        fs::write(workdir.join("d.txt"), "d").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("d.txt")).unwrap();
        let d_tree_id = index.write_tree().unwrap();
        let d_tree = repo.find_tree(d_tree_id).unwrap();
        let d = repo
            .commit(None, &sig, &sig, "D", &d_tree, &[&a_commit])
            .unwrap();
        // Reset the index so the next HEAD commits don't carry d.txt.
        index.remove_path(Path::new("d.txt")).unwrap();
        index.write().unwrap();
        fs::remove_file(workdir.join("d.txt")).unwrap();

        // Publish D as a remote tip.
        repo.reference("refs/remotes/origin/other", d, true, "test setup")
            .unwrap();

        // Advance HEAD with B and C off A.
        add_commit_on_head(&repo, "b.txt", "b");
        add_commit_on_head(&repo, "c.txt", "c");

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn fingerprint_changes_when_other_branch_moves() {
        // Reproduces the worktree case: a commit lands on a branch that is not
        // the checked-out HEAD. The root HEAD stays put, but the graph renders
        // that branch, so its fingerprint must change to drive a reload.
        let (_tmp, repo) = init_temp_repo();
        let base = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &base, false).unwrap();

        let before = graph_refs_fingerprint(&repo);
        let head_before = repo.head().unwrap().target();

        // New commit on `feature` only — HEAD (master/main) is never updated,
        // exactly as a commit made inside a linked worktree would behave.
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree = repo.find_tree(base.tree_id()).unwrap();
        let moved = repo
            .commit(None, &sig, &sig, "on feature", &tree, &[&base])
            .unwrap();
        repo.reference("refs/heads/feature", moved, true, "move feature")
            .unwrap();

        let after = graph_refs_fingerprint(&repo);

        assert_ne!(
            before.local, after.local,
            "a moved local branch must change the local refs fingerprint"
        );
        assert_eq!(
            repo.head().unwrap().target(),
            head_before,
            "the checked-out HEAD must be untouched by the other-branch commit"
        );
    }

    #[test]
    fn fingerprint_changes_when_branch_added_at_existing_commit() {
        // A new branch at an already-rendered commit changes the labels the
        // graph draws even though no new OID appears, so names must be hashed.
        let (_tmp, repo) = init_temp_repo();
        let base = repo.head().unwrap().peel_to_commit().unwrap();

        let before = graph_refs_fingerprint(&repo);
        repo.branch("feature", &base, false).unwrap();
        let after = graph_refs_fingerprint(&repo);

        assert_ne!(before.local, after.local);
    }

    #[test]
    fn fingerprint_buckets_remote_separately_from_local() {
        // A remote-only move must not perturb the local bucket, so the local
        // filter can ignore fetches.
        let (_tmp, repo) = init_temp_repo();
        let base = repo.head().unwrap().peel_to_commit().unwrap();

        let before = graph_refs_fingerprint(&repo);
        repo.reference("refs/remotes/origin/feature", base.id(), true, "remote tip")
            .unwrap();
        let after = graph_refs_fingerprint(&repo);

        assert_eq!(
            before.local, after.local,
            "remote move must not touch local"
        );
        assert_ne!(before.remote, after.remote, "remote bucket must change");
    }

    #[test]
    fn query_status_detects_commit_in_linked_worktree() {
        // End-to-end through the public entry point with a real linked worktree:
        // a commit made inside the worktree (on its own branch) must change the
        // root's `refs` fingerprint while leaving the root's checked-out HEAD
        // untouched — exactly the case the graph reload gate previously missed.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("root");
        let wt = tmp.path().join("wt-feature");
        fs::create_dir_all(&root).unwrap();

        let git = |args: &[&str], cwd: &Path| -> bool {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };

        if !git(&["init", "-q"], &root) {
            eprintln!("skipping query_status_detects_commit_in_linked_worktree: git unavailable");
            return;
        }
        assert!(git(&["commit", "-q", "--allow-empty", "-m", "init"], &root));
        // Linked worktree checked out on its own branch (shares root refs/heads).
        assert!(git(
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feature"
            ],
            &root,
        ));

        let before = query_status(&root, &SubmoduleConfig::default()).unwrap();

        // Commit inside the worktree only — the root's HEAD never moves.
        assert!(git(
            &["commit", "-q", "--allow-empty", "-m", "in worktree"],
            &wt,
        ));

        let after = query_status(&root, &SubmoduleConfig::default()).unwrap();

        assert_eq!(
            before.head_oid, after.head_oid,
            "root checked-out HEAD must be unchanged by the worktree commit"
        );
        assert_ne!(
            before.refs.local, after.refs.local,
            "worktree commit moved a shared branch tip; the local refs \
             fingerprint must change so the graph reloads"
        );
    }
}
