use super::*;

/// A repo with one commit and its own identity, no signing, and a private
/// hooks directory, so the developer's global Git config can't leak in.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path();
    run(path, &["init", "-q"]);
    run(path, &["config", "user.name", "Test"]);
    run(path, &["config", "user.email", "test@example.com"]);
    run(path, &["config", "commit.gpgsign", "false"]);
    run(path, &["config", "core.hooksPath", "hooks"]);
    std::fs::create_dir(path.join("hooks")).unwrap();
    std::fs::write(path.join(".gitignore"), "hooks/\n").unwrap();
    std::fs::write(path.join("a.txt"), "a\n").unwrap();
    std::fs::write(path.join("b.txt"), "b\n").unwrap();
    run(path, &["add", "--all"]);
    run(path, &["commit", "-q", "-m", "initial"]);
    tmp
}

fn run(path: &Path, args: &[&str]) -> String {
    let out = crate::git::process::git_command(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn head(path: &Path) -> String {
    run(path, &["rev-parse", "HEAD"])
}

#[test]
fn commits_only_the_staged_changes_with_the_exact_message() {
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = fixture();
    let path = tmp.path();
    std::fs::write(path.join("a.txt"), "a changed\n").unwrap();
    std::fs::write(path.join("b.txt"), "b changed\n").unwrap();
    run(path, &["add", "a.txt"]);

    // A leading dash and a `#` line would break `-m` or a stripping cleanup.
    let message = "-v is not a flag\n\nWhy it changed.\n# kept";
    let summary = commit(path, message, false).unwrap();

    let hash = run(path, &["log", "-1", "--format=%h"]);
    assert_eq!(summary, format!("{} -v is not a flag", hash.trim()));
    assert_eq!(
        run(path, &["log", "-1", "--format=%B"]),
        format!("{message}\n\n")
    );
    assert_eq!(run(path, &["status", "--porcelain"]), " M b.txt\n");
}

#[test]
fn stage_all_commits_modified_and_untracked_files() {
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = fixture();
    let path = tmp.path();
    std::fs::write(path.join("a.txt"), "a changed\n").unwrap();
    std::fs::write(path.join("c.txt"), "new\n").unwrap();

    commit(path, "Change a and add c", true).unwrap();

    assert_eq!(run(path, &["status", "--porcelain"]), "");
    assert_eq!(
        run(path, &["show", "--name-only", "--format=", "HEAD"]),
        "a.txt\nc.txt\n"
    );
}

#[test]
fn nothing_staged_fails_and_leaves_head() {
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = fixture();
    let path = tmp.path();
    std::fs::write(path.join("a.txt"), "a changed\n").unwrap();
    let before = head(path);

    let error = commit(path, "Nothing to see", false).unwrap_err();

    assert!(!error.is_empty());
    assert_eq!(head(path), before);
}

#[cfg(unix)]
#[test]
fn rejecting_hook_shows_its_output_and_keeps_the_change_staged() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = fixture();
    let path = tmp.path();
    let hook = path.join("hooks").join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'lint: trailing space in a.txt' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(path.join("a.txt"), "a changed \n").unwrap();
    run(path, &["add", "a.txt"]);
    let before = head(path);

    let error = commit(path, "Change a", false).unwrap_err();

    assert!(error.contains("lint: trailing space in a.txt"), "{error}");
    assert_eq!(head(path), before);
    assert_eq!(run(path, &["status", "--porcelain"]), "M  a.txt\n");
}

#[cfg(unix)]
#[test]
fn draft_returns_trimmed_output_and_reports_failures() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path();
    std::fs::write(path.join("marker"), "").unwrap();

    // Runs in the checkout: the command can see its files.
    assert_eq!(
        draft(
            path,
            "test -f marker && printf '  Add marker\\n\\nBody.\\n\\n'"
        ),
        Ok("Add marker\n\nBody.".to_string())
    );

    let error = draft(path, "echo 'no agent configured' >&2; exit 3").unwrap_err();
    assert!(error.contains("no agent configured"), "{error}");

    assert!(draft(path, "true").is_err());
}
