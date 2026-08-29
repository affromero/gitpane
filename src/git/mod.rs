pub(crate) mod commit_files;
pub(crate) mod github;
pub(crate) mod graph;
pub(crate) mod graph_render;
pub(crate) mod process;
pub(crate) mod scanner;
pub(crate) mod status;

use std::io;
use std::sync::OnceLock;

/// Turn a process-spawn error into a user-facing string, special-casing a
/// missing `git` binary. Without this the status bar shows a raw
/// `No such file or directory (os error 2)`, which gives no hint that the
/// real problem is that `git` is not installed.
pub(crate) fn describe_spawn_error(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        "git is not installed or not on PATH".to_string()
    } else {
        e.to_string()
    }
}

/// Whether the `git` executable is available on PATH. Probed once (via
/// `git --version`) and cached for the process lifetime.
///
/// gitpane reads all repo state through libgit2, so a missing binary does not
/// stop it from running; it only disables the CLI-backed actions (fetch,
/// pull, submodule operations, and diffs).
pub(crate) fn git_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// The remote gitpane should treat as canonical for `repo` when nothing more
/// specific is configured: `origin` if present, else the lexicographically
/// first remote (stable even when config order is not). Gerrit and mirror
/// workspaces name their remote `gerrit` / `gitea_mirror` etc. and have no
/// `origin` at all, so hard-coding `origin` breaks them. `None` when the repo
/// has no remotes.
pub(crate) fn preferred_remote(repo: &git2::Repository) -> Option<String> {
    let mut remotes: Vec<String> = repo
        .remotes()
        .ok()?
        .iter()
        .filter_map(|r| r.ok().flatten().map(|s| s.to_string()))
        .collect();
    if remotes.iter().any(|r| r == "origin") {
        return Some("origin".into());
    }
    remotes.sort();
    remotes.into_iter().next()
}

/// The explicit `<remote>` to append to a `git pull` / `git push` of `branch`
/// at `path`, or `None` when the command should run bare.
///
/// `None` covers two cases the caller treats identically (append nothing):
/// git can already resolve the destination itself — a configured upstream
/// (`branch.<name>.remote` + `.merge`), or for pushes `branch.<name>.pushRemote`
/// / `remote.pushDefault` — in which case a bare command also honors renamed
/// upstream branches (local `main` tracking `gerrit/master`) that an explicit
/// `<remote> <branch>` would break; or the repo has no remotes at all and git
/// gets to report that.
///
/// Explicit fallback order: the branch's own `branch.<name>.remote` when it
/// names a real remote (the user's intent even without a `.merge` ref), else
/// [`preferred_remote`].
///
/// Blocking (opens the repo) — call from `spawn_blocking`.
pub(crate) fn resolve_sync_remote(
    path: &std::path::Path,
    branch: &str,
    push: bool,
) -> Option<String> {
    let repo = git2::Repository::open(path).ok()?;
    let config = repo.config().ok()?;
    let get = |key: &str| config.get_string(key).ok();

    let upstream = get(&format!("branch.{branch}.remote"));
    let has_merge = get(&format!("branch.{branch}.merge")).is_some();
    let has_push_dest = push
        && (get(&format!("branch.{branch}.pushremote")).is_some()
            || get("remote.pushdefault").is_some());
    if (upstream.is_some() && has_merge) || has_push_dest {
        return None;
    }
    if let Some(remote) = upstream
        && repo.find_remote(&remote).is_ok()
    {
        return Some(remote);
    }
    preferred_remote(&repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_spawn_error_flags_missing_git() {
        let err = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");
        assert_eq!(
            describe_spawn_error(&err),
            "git is not installed or not on PATH"
        );
    }

    #[test]
    fn describe_spawn_error_passes_through_other_errors() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
        assert_eq!(describe_spawn_error(&err), "permission denied");
    }

    /// A git2-native temp repo with the given remotes configured — no `git`
    /// binary involved, so these tests never skip.
    fn repo_with_remotes(remotes: &[&str]) -> (tempfile::TempDir, git2::Repository) {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        for (i, name) in remotes.iter().enumerate() {
            repo.remote(name, &format!("https://example.com/repo{i}.git"))
                .unwrap();
        }
        (tmp, repo)
    }

    #[test]
    fn sync_remote_prefers_origin_without_upstream() {
        let (tmp, _repo) = repo_with_remotes(&["origin", "gerrit"]);
        assert_eq!(
            resolve_sync_remote(tmp.path(), "main", false).as_deref(),
            Some("origin")
        );
    }

    #[test]
    fn sync_remote_uses_sole_remote_when_no_origin() {
        // Gerrit/mirror workspaces: no origin, a single `gerrit` remote.
        let (tmp, _repo) = repo_with_remotes(&["gerrit"]);
        assert_eq!(
            resolve_sync_remote(tmp.path(), "main", false).as_deref(),
            Some("gerrit")
        );
    }

    #[test]
    fn sync_remote_picks_first_lexicographic_without_origin() {
        let (tmp, _repo) = repo_with_remotes(&["zzz", "gerrit"]);
        // No origin → lexicographically first, not config order.
        assert_eq!(
            resolve_sync_remote(tmp.path(), "main", false).as_deref(),
            Some("gerrit")
        );
    }

    #[test]
    fn sync_remote_none_without_remotes() {
        let (tmp, _repo) = repo_with_remotes(&[]);
        assert_eq!(resolve_sync_remote(tmp.path(), "main", false), None);
    }

    #[test]
    fn sync_remote_defers_to_configured_upstream() {
        // Renamed upstream (local main tracking gerrit/master): a bare
        // `git pull` resolves it correctly; an explicit `gerrit main` would
        // fetch a nonexistent ref, so the resolver must return None.
        let (tmp, repo) = repo_with_remotes(&["origin", "gerrit"]);
        let mut config = repo.config().unwrap();
        config.set_str("branch.main.remote", "gerrit").unwrap();
        config
            .set_str("branch.main.merge", "refs/heads/master")
            .unwrap();
        assert_eq!(resolve_sync_remote(tmp.path(), "main", false), None);
        assert_eq!(resolve_sync_remote(tmp.path(), "main", true), None);
    }

    #[test]
    fn sync_remote_prefers_branch_remote_over_origin() {
        // Partial upstream (remote without a merge ref): still the user's
        // configured intent, so it beats the origin default.
        let (tmp, repo) = repo_with_remotes(&["origin", "gerrit"]);
        let mut config = repo.config().unwrap();
        config.set_str("branch.main.remote", "gerrit").unwrap();
        assert_eq!(
            resolve_sync_remote(tmp.path(), "main", false).as_deref(),
            Some("gerrit")
        );
    }

    #[test]
    fn sync_remote_defers_to_push_remote_only_for_push() {
        let (tmp, repo) = repo_with_remotes(&["origin", "gerrit"]);
        let mut config = repo.config().unwrap();
        config.set_str("branch.main.pushremote", "gerrit").unwrap();
        assert_eq!(resolve_sync_remote(tmp.path(), "main", true), None);
        // Pull ignores push config and still gets an explicit remote.
        assert_eq!(
            resolve_sync_remote(tmp.path(), "main", false).as_deref(),
            Some("origin")
        );
    }
}
