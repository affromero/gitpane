#![cfg(unix)]

use super::*;
use std::fs;

fn git(path: &Path, args: &[&str]) -> String {
    let output = crate::git::process::git_command(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git2::Repository::init(path).unwrap();
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "user.email", "test@example.invalid"]);
}

fn commit(path: &Path, contents: &str) -> String {
    fs::write(path.join("file"), contents).unwrap();
    git(path, &["add", "file"]);
    git(path, &["commit", "-m", contents]);
    git(path, &["rev-parse", "HEAD"])
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let remote = root.join("remote.git");
    let source = root.join("source");
    let repo = root.join("workspace");
    git2::Repository::init_bare(&remote).unwrap();
    init_repo(&source);
    commit(&source, "initial");
    let branch = git(&source, &["symbolic-ref", "--short", "HEAD"]);
    git(
        &source,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&source, &["push", "-u", "origin", &branch]);
    git(
        &root,
        &["clone", remote.to_str().unwrap(), repo.to_str().unwrap()],
    );
    (temp, root, source, repo, branch)
}

fn snapshot(scope: &Scope, repo: &Path) -> (String, String) {
    let response = preview(
        scope,
        Preview {
            repo: repo.to_owned(),
        },
    )
    .unwrap();
    (
        response["repo_identity"].as_str().unwrap().to_owned(),
        response["fetch_token"].as_str().unwrap().to_owned(),
    )
}

fn fetch(scope: &Scope, repo: &Path, tokens: (String, String)) -> Result<Value, BridgeError> {
    execute(
        scope,
        Fetch {
            repo: repo.to_owned(),
            expected_repo_identity: tokens.0,
            expected_fetch_token: tokens.1,
        },
    )
}

#[test]
fn fetch_updates_tracking_refs_and_leaves_local_head_unchanged() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, root, source, repo, branch) = setup();
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    let before = git(&repo, &["rev-parse", "HEAD"]);
    let after = commit(&source, "second");
    git(&source, &["push", "origin", &branch]);
    let tokens = snapshot(&scope, &repo);
    fetch(&scope, &repo, tokens).unwrap();
    assert_eq!(
        git(
            &repo,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        after
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
    assert!(scope.repository(&root.join("source")).is_err());
}

#[test]
fn fetch_uses_each_worktrees_default_remote() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, root, source, repo, branch) = setup();
    let other = root.join("other.git");
    let linked = repo.join("linked");
    git2::Repository::init_bare(&other).unwrap();
    git(
        &source,
        &["remote", "add", "other", other.to_str().unwrap()],
    );
    git(&source, &["push", "other", &branch]);
    git(&repo, &["remote", "add", "other", other.to_str().unwrap()]);
    git(&repo, &["fetch", "other"]);
    git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    git(&linked, &["config", "branch.linked.remote", "other"]);
    git(
        &linked,
        &[
            "config",
            "branch.linked.merge",
            &format!("refs/heads/{branch}"),
        ],
    );
    let before = git(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{branch}")],
    );
    let after = commit(&source, "advanced");
    git(&source, &["push", "origin", &branch]);
    git(&source, &["push", "other", &branch]);
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    fetch(&scope, &linked, snapshot(&scope, &linked)).unwrap();
    assert_eq!(
        git(
            &repo,
            &["rev-parse", &format!("refs/remotes/other/{branch}")]
        ),
        after
    );
    assert_eq!(
        git(
            &repo,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        before
    );
}

#[test]
fn fetch_handles_unborn_and_detached_head() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, root, _source, repo, branch) = setup();
    let unborn = root.join("unborn");
    init_repo(&unborn);
    git(
        &unborn,
        &[
            "remote",
            "add",
            "origin",
            root.join("remote.git").to_str().unwrap(),
        ],
    );
    let scope = Scope::new(vec![root]).unwrap();
    fetch(&scope, &unborn, snapshot(&scope, &unborn)).unwrap();
    assert!(
        !git(
            &unborn,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        )
        .is_empty()
    );
    git(&repo, &["checkout", "--detach"]);
    fetch(&scope, &repo, snapshot(&scope, &repo)).unwrap();
}

#[test]
fn fetch_revalidates_identity_config_and_symbolic_branch_but_not_head_oid() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, _root, _source, repo, _) = setup();
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    let tokens = snapshot(&scope, &repo);
    commit(&repo, "local change");
    fetch(&scope, &repo, tokens).unwrap();
    let tokens = snapshot(&scope, &repo);
    git(&repo, &["config", "remote.origin.url", "/different/path"]);
    assert_eq!(
        fetch(&scope, &repo, tokens).unwrap_err().code,
        "stale_target"
    );
    let tokens = snapshot(&scope, &repo);
    git(&repo, &["checkout", "-b", "other-branch"]);
    assert_eq!(
        fetch(&scope, &repo, tokens).unwrap_err().code,
        "stale_target"
    );
    let tokens = snapshot(&scope, &repo);
    fs::rename(repo.join(".git"), repo.join(".old-git")).unwrap();
    git2::Repository::init(&repo).unwrap();
    assert_eq!(
        fetch(&scope, &repo, tokens).unwrap_err().code,
        "stale_target"
    );
}

#[test]
fn fetch_surfaces_remote_errors_and_rejects_scope_escape() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, root, _source, repo, _) = setup();
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            root.join("missing").to_str().unwrap(),
        ],
    );
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    let error = fetch(&scope, &repo, snapshot(&scope, &repo)).unwrap_err();
    assert_eq!(error.code, "git_error");
    assert!(error.message.contains("remote"));
    assert_eq!(
        preview(
            &scope,
            Preview {
                repo: root.join("source")
            }
        )
        .unwrap_err()
        .code,
        "outside_workspace"
    );
}

#[test]
fn fetch_failure_hints_are_specific_without_exposing_untrusted_diagnostics() {
    let cases: &[(&str, &str)] = &[
        (
            "fatal: Authentication failed for https://user:secret@example.invalid/private",
            "authentication",
        ),
        ("fatal: Could not resolve host: secret-token", "resolve"),
        (
            "fatal: repository not found https://user:secret@example.invalid/",
            "find the remote",
        ),
        (
            "error: cannot lock ref refs/remotes/secret",
            "remote-tracking",
        ),
        (
            "fatal: connection refused while accessing secret",
            "connect",
        ),
        (
            "helper printed secret-token outside any URL",
            "Check the remote",
        ),
    ];
    for (input, expected) in cases {
        let hint = failure_hint(input.as_bytes());
        assert!(
            hint.to_lowercase().contains(&expected.to_lowercase()),
            "{hint}"
        );
        assert!(!hint.contains("secret"));
        assert!(!hint.contains("example.invalid"));
    }
}

#[test]
fn fetch_rejects_changed_per_worktree_configuration() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, _root, _source, repo, _) = setup();
    let linked = repo.join("linked");
    git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let scope = Scope::new(vec![repo]).unwrap();
    let tokens = snapshot(&scope, &linked);
    git(
        &linked,
        &[
            "config",
            "--worktree",
            "remote.origin.url",
            "/changed/worktree/remote",
        ],
    );
    assert_eq!(
        fetch(&scope, &linked, tokens).unwrap_err().code,
        "stale_target"
    );
}

#[test]
fn fetch_does_not_recurse_into_submodules_even_when_configured() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, root, _source, repo, _) = setup();
    let child_remote = root.join("child.git");
    let child_source = root.join("child-source");
    git2::Repository::init_bare(&child_remote).unwrap();
    init_repo(&child_source);
    commit(&child_source, "child initial");
    let branch = git(&child_source, &["symbolic-ref", "--short", "HEAD"]);
    git(
        &child_source,
        &["remote", "add", "origin", child_remote.to_str().unwrap()],
    );
    git(&child_source, &["push", "-u", "origin", &branch]);
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            child_remote.to_str().unwrap(),
            "child",
        ],
    );
    let child = repo.join("child");
    let before = git(
        &child,
        &["rev-parse", &format!("refs/remotes/origin/{branch}")],
    );
    let after = commit(&child_source, "child advanced");
    git(&child_source, &["push", "origin", &branch]);
    git(&repo, &["config", "fetch.recurseSubmodules", "true"]);
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    fetch(&scope, &repo, snapshot(&scope, &repo)).unwrap();
    assert_eq!(
        git(
            &child,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        before
    );
    assert_ne!(before, after);
}
