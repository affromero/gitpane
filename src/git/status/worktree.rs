use crate::config::SubmoduleConfig;
use git2::Repository;
use std::path::Path;

use super::WorktreeEntry;
use super::change_summary::{ChangeSummary, collect_change_summary};

/// Collect details for each linked worktree using the git2 API.
/// Mirrors the pattern in `git/graph.rs::collect_worktree_branches`.
pub(super) fn collect_worktree_info(
    repo: &Repository,
    sub_cfg: &SubmoduleConfig,
) -> Vec<WorktreeEntry> {
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
///
/// The fetch runs in its own process group so a timeout kill takes the whole
/// tree down with it: `git fetch` over SSH leaves a detached `ssh`
/// (git-upload-pack) child behind if only the git process is killed, and that
/// orphan keeps the server-side connection alive until the network gives up.
pub(super) fn fetch_remote_silent(path: &Path) -> bool {
    use wait_timeout::ChildExt;

    let child = fetch_child(path);

    match child {
        Ok(mut c) => {
            match c.wait_timeout(std::time::Duration::from_secs(30)) {
                Ok(Some(status)) => status.success(),
                Ok(None) => {
                    // Timed out — kill the whole process group (git + its ssh
                    // children), not just git.
                    kill_process_group(&mut c);
                    let _ = c.wait();
                    false
                }
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// Spawn `git fetch --quiet` with stdout/stderr discarded.
///
/// On unix the child gets its own process group so that a timeout kill can
/// target the whole tree (git and every child it spawned, notably `ssh` for
/// ssh remotes) instead of leaving orphans behind.
fn fetch_child(path: &Path) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .arg("fetch")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group (pgid == pid) so kill_process_group can take
        // git's children — the ssh client — down with it.
        cmd.process_group(0);
    }
    cmd.spawn()
}

/// Kill the child and everything in its process group.
///
/// On unix the fetch child was spawned with `process_group(0)`, so its pgid
/// equals its pid and this takes `git fetch` and its `ssh` child together.
/// Non-unix platforms fall back to killing only the child itself (previous
/// behavior).
#[cfg(unix)]
pub(super) fn kill_process_group(child: &mut std::process::Child) {
    // SAFETY: child.id() is the pgid because fetch_child spawned it with
    // process_group(0); SIGKILL cannot fail for a valid existing pgid, and a
    // race where the group already exited is harmless (ESRCH is ignored).
    let pgid = child.id() as i32;
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(super) fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
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
pub(super) fn compute_ahead_behind(repo: &Repository) -> (usize, usize) {
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
