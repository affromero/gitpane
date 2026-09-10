pub(crate) mod commit_files;
pub(crate) mod file_ops;
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

/// Skip CLI integration tests only when Git is missing. A broken installation
/// must fail the test instead of silently reducing coverage.
#[cfg(test)]
pub(crate) fn git_test_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *AVAILABLE.get_or_init(|| {
        match std::process::Command::new("git").arg("--version").output() {
            Ok(output) => {
                assert!(
                    output.status.success(),
                    "git --version failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => panic!("cannot execute git --version: {error}"),
        }
    });
    if !available {
        eprintln!(
            "skipping {}: git is not on PATH; install Git to run this integration test",
            std::thread::current()
                .name()
                .unwrap_or("Git integration test")
        );
    }
    available
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

/// Keep repository and branch operands literal when selecting a sync remote.
pub(crate) fn sync_arguments(
    path: &std::path::Path,
    branch: &str,
    push: bool,
    mut args: Vec<String>,
) -> Vec<String> {
    if !branch.is_empty()
        && branch != "(no branch)"
        && branch != "HEAD"
        && let Some(remote) = resolve_sync_remote(path, branch, push)
    {
        args.extend(["--".into(), remote, branch.into()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn assert_git_dependency_failure(directory: &std::path::Path, message: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::file_ops::tests::tracked_diff_includes_staged_and_unstaged_content_only_for_selected_file",
                "--nocapture",
            ])
            .env("PATH", directory)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(101));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
    }

    #[cfg(unix)]
    #[test]
    fn git_integration_tests_fail_when_git_exits_unsuccessfully() {
        let directory = tempfile::tempdir().unwrap();
        // The test harness rejects `--version`, providing an executable that
        // exits unsuccessfully without needing a shell or a Git installation.
        std::os::unix::fs::symlink(
            std::env::current_exe().unwrap(),
            directory.path().join("git"),
        )
        .unwrap();
        assert_git_dependency_failure(directory.path(), "git --version failed");
    }

    #[cfg(unix)]
    #[test]
    fn git_integration_tests_fail_when_git_is_not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let git = directory.path().join("git");
        std::fs::write(&git, "").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_git_dependency_failure(directory.path(), "cannot execute git --version");
    }

    #[cfg(unix)]
    #[test]
    fn hook_environment_cannot_redirect_repository_operations() {
        if !git_test_available() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        repo.index().unwrap().write().unwrap();
        std::fs::write(directory.path().join("sentinel"), "preserve this checkout").unwrap();
        let snapshot = || {
            walkdir::WalkDir::new(directory.path())
                .into_iter()
                .map(Result::unwrap)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| {
                    let path = entry.into_path();
                    let contents = std::fs::read(&path).unwrap();
                    (path, contents)
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = snapshot();
        let tests = [
            "git::file_ops::tests::selected_pattern_filename_does_not_change_other_files_or_index_entries",
            "git::file_ops::tests::deleting_untracked_pattern_filename_preserves_other_files",
            "git::file_ops::tests::rename_actions_update_both_paths_and_preserve_other_staged_changes",
            "git::file_ops::tests::selected_diff_excludes_other_pattern_matches_in_worktree_and_commit",
            "git::file_ops::tests::untracked_diff_reads_from_selected_repository",
            "git::file_ops::tests::tracked_diff_includes_staged_and_unstaged_content_only_for_selected_file",
            "git::file_ops::tests::discarding_staged_addition_before_first_commit_preserves_other_changes",
            "git::tests::pushing_to_a_flag_named_remote_cannot_force_divergent_history",
            "git::file_ops::tests::staging_rename_does_not_stage_a_recreated_source_file",
            "git::file_ops::tests::discarding_rename_refuses_to_overwrite_a_recreated_source",
            "git::file_ops::tests::discarding_rename_preserves_a_recreated_dangling_symlink",
            "app::launch::tests::configured_file_open_passes_file_to_editor_with_directory_cwd",
            "app::launch::tests::placement_picker_preserves_file_target_and_directory_cwd",
            "git::process::tests::run_git_op_capturing_registers_and_unregisters",
            "git::status::tests_refs::query_status_detects_commit_in_linked_worktree",
            "watcher::tests::watch_dirs_respects_gitignore",
        ];
        // Every injected path refers to this sacrificial repository. Even a
        // regression must never direct a test operation at the real checkout.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .args(tests)
            .current_dir(directory.path())
            .env("GIT_DIR", repo.path())
            .env("GIT_WORK_TREE", directory.path())
            .env("GIT_INDEX_FILE", repo.path().join("index"))
            .env("GIT_COMMON_DIR", repo.path())
            .env("GIT_OBJECT_DIRECTORY", repo.path().join("objects"))
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_PREFIX")
            .env_remove("GIT_NAMESPACE")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            snapshot() == before,
            "test operations modified the inherited repository\n{stdout}\n{stderr}"
        );
        assert!(output.status.success(), "{stdout}\n{stderr}");
        for name in tests {
            assert!(
                stdout.contains(&format!("test {name} ... ok")),
                "expected integration test did not pass: {name}\n{stdout}\n{stderr}"
            );
        }
    }

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
    fn pushing_to_a_flag_named_remote_cannot_force_divergent_history() {
        if !git_test_available() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let local_path = tmp.path().join("local");
        let remote_path = tmp.path().join("remote");
        let repo = git2::Repository::init(&local_path).unwrap();
        let remote = git2::Repository::init_bare(&remote_path).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        repo.config()
            .unwrap()
            .set_str("push.default", "current")
            .unwrap();
        for name in ["--force", "main"] {
            repo.remote(name, remote_path.to_str().unwrap()).unwrap();
        }
        let commit = |text: &str| {
            std::fs::write(local_path.join("file"), text).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("file")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("Test", "test@example.com").unwrap();
            let parents: Vec<_> = repo
                .head()
                .ok()
                .map(|head| head.peel_to_commit().unwrap())
                .into_iter()
                .collect();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                text,
                &tree,
                &parents.iter().collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let first = commit("first");
        let remote_tip = commit("remote change");
        let initial_push = process::git_command(&local_path)
            .args(["push", "--", "--force", "main"])
            .output()
            .unwrap();
        assert!(
            initial_push.status.success(),
            "{}",
            String::from_utf8_lossy(&initial_push.stderr)
        );
        repo.reset(
            &repo.find_object(first, None).unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
        let local_tip = commit("divergent local change");
        let args = sync_arguments(&local_path, "main", true, vec!["push".into()]);
        let output = process::git_command(&local_path)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("rejected"));
        assert_eq!(remote.refname_to_id("refs/heads/main").unwrap(), remote_tip);
        assert_eq!(repo.head().unwrap().target(), Some(local_tip));
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
