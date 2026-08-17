pub(crate) mod commit_files;
pub(crate) mod github;
pub(crate) mod graph;
pub(crate) mod graph_render;
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

/// The remote to target for a `git pull` / `git push` against `path`.
///
/// gitpane passes an explicit `<remote> <branch>` so pull/push work even
/// without an `upstream` configured, but it should not hard-code `origin`:
/// Gerrit and mirror workspaces name their remote `gerrit` / `gitea_mirror`
/// etc. and have no `origin` at all, which would make every pull/push fail
/// with "fatal: 'origin' does not appear to be a git repository".
///
/// Resolution order: `origin` if present, else the sole remote, else the
/// first remote in lexicographic order (stable even when config order is
/// not). `None` when the repo has no remotes (the caller then omits the
/// remote and lets `git pull` / `git push` use the repo's own upstream, or
/// report the missing remote).
///
/// Blocking (opens the repo) — call from `spawn_blocking`.
pub(crate) fn resolve_remote_name(path: &std::path::Path) -> Option<String> {
    use git2::Repository;

    let repo = Repository::open(path).ok()?;
    let mut remotes: Vec<String> = repo
        .remotes()
        .ok()?
        .iter()
        .filter_map(|r| r.ok().flatten().map(|s| s.to_string()))
        .collect();
    if remotes.is_empty() {
        return None;
    }
    if let Some(pos) = remotes.iter().position(|r| r == "origin") {
        return Some(remotes.swap_remove(pos));
    }
    if remotes.len() == 1 {
        return Some(remotes.pop().unwrap());
    }
    remotes.sort();
    Some(remotes.into_iter().next().unwrap())
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

    /// A real git repo (via `git init`) with remotes configured through
    /// `git remote add`, so `resolve_remote_name` has a real config to read.
    /// Returns `None` when `git` is unavailable so the suite degrades
    /// gracefully under environments without git. The `TempDir` is returned
    /// alongside the path so the repo outlives the test body.
    fn repo_with_remotes(remotes: &[&str]) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        let tmp = tempfile::TempDir::new().ok()?;
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .ok()?;
        if !init.success() {
            return None;
        }
        for (i, name) in remotes.iter().enumerate() {
            let add = std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    name,
                    &format!("https://example.com/repo{i}.git"),
                ])
                .current_dir(tmp.path())
                .status()
                .ok()?;
            if !add.success() {
                return None;
            }
        }
        let dir = tmp.path().to_path_buf();
        Some((tmp, dir))
    }

    #[test]
    fn resolve_remote_prefers_origin() {
        let Some((_tmp, dir)) = repo_with_remotes(&["origin", "gerrit"]) else {
            eprintln!("skipping: git unavailable");
            return;
        };
        assert_eq!(resolve_remote_name(&dir).as_deref(), Some("origin"));
    }

    #[test]
    fn resolve_remote_uses_sole_remote_when_no_origin() {
        // Gerrit/mirror workspaces: no origin, a single `gerrit` remote.
        let Some((_tmp, dir)) = repo_with_remotes(&["gerrit"]) else {
            eprintln!("skipping: git unavailable");
            return;
        };
        assert_eq!(resolve_remote_name(&dir).as_deref(), Some("gerrit"));
    }

    #[test]
    fn resolve_remote_picks_first_lexicographic_without_origin() {
        let Some((_tmp, dir)) = repo_with_remotes(&["zzz", "gerrit"]) else {
            eprintln!("skipping: git unavailable");
            return;
        };
        // No origin → lexicographically first, not config order.
        assert_eq!(resolve_remote_name(&dir).as_deref(), Some("gerrit"));
    }

    #[test]
    fn resolve_remote_none_without_remotes() {
        let Some((_tmp, dir)) = repo_with_remotes(&[]) else {
            eprintln!("skipping: git unavailable");
            return;
        };
        assert_eq!(resolve_remote_name(&dir), None);
    }
}
