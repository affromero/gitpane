pub(crate) mod commit_files;
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
}
