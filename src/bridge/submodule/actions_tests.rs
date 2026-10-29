use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use git2::{Oid, Repository, Signature};
use serde_json::json;

use super::{Request, execute};
use crate::bridge::scope::Scope;

fn commit_file(repo: &Repository, name: &str, contents: &str) -> Oid {
    std::fs::write(repo.workdir().unwrap().join(name), contents).unwrap();
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &tree,
        &parents,
    )
    .unwrap()
}

fn fixture(names: &[&str]) -> (tempfile::TempDir, PathBuf, Vec<Oid>, Scope) {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let parent_path = workspace.join("parent");
    std::fs::create_dir_all(&parent_path).unwrap();
    let parent = Repository::init(&parent_path).unwrap();
    commit_file(&parent, "parent.txt", "parent");
    let mut old = Vec::new();
    for name in names {
        let source_path = temporary.path().join(format!("source-{name}"));
        std::fs::create_dir(&source_path).unwrap();
        let source = Repository::init(&source_path).unwrap();
        old.push(commit_file(&source, "file.txt", "old"));
        commit_file(&source, "file.txt", "new");
        let mut sub = parent
            .submodule(source_path.to_str().unwrap(), Path::new(name), true)
            .unwrap();
        sub.clone(None).unwrap();
        sub.add_finalize().unwrap();
    }
    commit_file(&parent, "parent.txt", "parent");
    let scope = Scope::new(vec![workspace]).unwrap();
    (temporary, parent_path, old, scope)
}

fn request(
    parent: &Path,
    scope: &Scope,
    operation: &str,
) -> Result<serde_json::Value, crate::bridge::BridgeError> {
    let repo = Repository::open(parent).unwrap();
    let head = repo.head().unwrap();
    execute(
        scope,
        Request {
            repo: parent.to_path_buf(),
            operation: operation.into(),
            expected_branch: Some(head.shorthand().unwrap().to_owned()),
            expected_head: head.target().unwrap().to_string(),
        },
    )
}

fn point_index_at(parent: &Path, names: &[&str], oids: &[Oid]) {
    let repo = Repository::open(parent).unwrap();
    let mut index = repo.index().unwrap();
    for (name, oid) in names.iter().zip(oids) {
        let mut entry = index.get_path(Path::new(name), 0).unwrap();
        entry.id = *oid;
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
}

struct ReleaseOnDrop(PathBuf);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "release");
    }
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn delayed_batch_preserves_changed_child(change_branch: Option<bool>) {
    use std::os::unix::fs::PermissionsExt;
    let (temporary, parent, old, scope) = fixture(&["a", "b"]);
    point_index_at(&parent, &["a", "b"], &old);
    if change_branch.is_none() {
        Repository::open(&parent)
            .unwrap()
            .config()
            .unwrap()
            .set_str("submodule.b.update", "none")
            .unwrap();
    }
    let hooks = temporary.path().join("hooks");
    let started = temporary.path().join("checkout-started");
    let release = temporary.path().join("checkout-release");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    let script = format!(
        "#!/bin/sh\n: > {}\nwhile [ ! -e {} ]; do /bin/sleep 0.02; done\n",
        shell_path(&started),
        shell_path(&release)
    );
    std::fs::write(&hook, script).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = Repository::open(parent.join("a")).unwrap();
    first
        .config()
        .unwrap()
        .set_str("core.hooksPath", hooks.to_str().unwrap())
        .unwrap();

    std::thread::scope(|threads| {
        let release_on_drop = ReleaseOnDrop(release.clone());
        let pending = threads.spawn(|| request(&parent, &scope, "update"));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !started.exists() {
            assert!(
                Instant::now() < deadline,
                "first real Git checkout did not reach the hook"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let second = Repository::open(parent.join("b")).unwrap();
        let before = second.head().unwrap().peel_to_commit().unwrap();
        let original = before.id();
        if change_branch == Some(true) {
            second
                .branch("switched-during-batch", &before, false)
                .unwrap();
            second.set_head("refs/heads/switched-during-batch").unwrap();
            assert_eq!(second.head().unwrap().target(), Some(original));
        } else if change_branch == Some(false) {
            let tree = before.tree().unwrap();
            let sig = Signature::now("Test", "test@example.invalid").unwrap();
            let new = second
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "concurrent commit",
                    &tree,
                    &[&before],
                )
                .unwrap();
            assert_ne!(new, original);
        } else {
            Repository::open(&parent)
                .unwrap()
                .config()
                .unwrap()
                .remove("submodule.b.update")
                .unwrap();
        }
        let expected_head = second.head().unwrap().peel_to_commit().unwrap().id();
        let expected_ref = second
            .find_reference("HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .map(str::to_owned);
        drop(release_on_drop);
        let result = pending.join().unwrap().unwrap_err();
        assert_eq!(result.code, "partial_failure", "{}", result.message);
        assert!(
            result.message.contains("branch or HEAD changed"),
            "{}",
            result.message
        );
        assert_eq!(
            std::fs::read_to_string(parent.join("a/file.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(parent.join("b/file.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            second.head().unwrap().peel_to_commit().unwrap().id(),
            expected_head
        );
        assert_eq!(
            second
                .find_reference("HEAD")
                .unwrap()
                .symbolic_target()
                .unwrap()
                .map(str::to_owned),
            expected_ref
        );
    });
}

#[test]
fn update_rejects_child_branch_or_head_changed_during_an_earlier_checkout() {
    if !crate::git::git_test_available() {
        return;
    }
    delayed_batch_preserves_changed_child(Some(true));
    delayed_batch_preserves_changed_child(Some(false));
    delayed_batch_preserves_changed_child(None);
}

#[test]
fn direct_update_initializes_a_deinitialized_submodule_and_sync_reports_skips() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, parent, _old, scope) = fixture(&["child"]);
    let output = crate::git::process::git_command(&parent)
        .args(["submodule", "deinit", "-f", "--", "child"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request(&parent, &scope, "sync").unwrap(),
        json!({"completed": [], "skipped": ["child"]})
    );
    assert_eq!(
        request(&parent, &scope, "update").unwrap(),
        json!({"completed": ["child"], "skipped": []})
    );
    assert!(parent.join("child/file.txt").exists());
}

#[test]
fn direct_sync_updates_initialized_submodule_url_without_network() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temporary, parent, _old, scope) = fixture(&["child"]);
    let new_url = "https://example.invalid/updated.git";
    let mut modules = git2::Config::open(&parent.join(".gitmodules")).unwrap();
    modules.set_str("submodule.child.url", new_url).unwrap();
    assert_eq!(
        request(&parent, &scope, "sync").unwrap(),
        json!({"completed": ["child"], "skipped": []})
    );
    let parent_repo = Repository::open(&parent).unwrap();
    assert_eq!(
        parent_repo
            .config()
            .unwrap()
            .get_string("submodule.child.url")
            .unwrap(),
        new_url
    );
    let child_repo = Repository::open(parent.join("child")).unwrap();
    assert_eq!(
        child_repo.find_remote("origin").unwrap().url().unwrap(),
        new_url
    );
}

#[test]
fn recursive_update_initializes_newly_discovered_nested_submodule() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, parent, _, scope) = fixture(&["child"]);
    let nested_source = temporary.path().join("nested-source");
    std::fs::create_dir(&nested_source).unwrap();
    let nested = Repository::init(&nested_source).unwrap();
    commit_file(&nested, "nested.txt", "nested content");
    let child_path = parent.join("child");
    let child = Repository::open(&child_path).unwrap();
    let mut sub = child
        .submodule(nested_source.to_str().unwrap(), Path::new("nested"), true)
        .unwrap();
    sub.clone(None).unwrap();
    sub.add_finalize().unwrap();
    drop(sub);
    let child_head = commit_file(&child, "file.txt", "new");
    point_index_at(&parent, &["child"], &[child_head]);
    let output = crate::git::process::git_command(&child_path)
        .args(["submodule", "deinit", "-f", "--", "nested"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut modules = git2::Config::open(&child_path.join(".gitmodules")).unwrap();
    modules
        .set_str("submodule.nested.update", "!unsafe-command")
        .unwrap();
    let problem = request(&parent, &scope, "update").unwrap_err();
    assert_eq!(problem.code, "repository_error", "{}", problem.message);
    assert!(
        problem.message.contains("submodule 'update'"),
        "{}",
        problem.message
    );
    assert_eq!(
        child.head().unwrap().peel_to_commit().unwrap().id(),
        child_head
    );
    assert!(!child_path.join("nested/nested.txt").exists());
    modules.remove("submodule.nested.update").unwrap();
    let result = request(&parent, &scope, "update").unwrap();
    assert_eq!(
        result,
        json!({"completed": ["child", "child/nested"], "skipped": []})
    );
    assert_eq!(
        std::fs::read_to_string(child_path.join("nested/nested.txt")).unwrap(),
        "nested content"
    );
}

#[test]
fn pull_latest_updates_initialized_direct_child_using_its_remote() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, parent, _, scope) = fixture(&["child"]);
    let child = Repository::open(parent.join("child")).unwrap();
    let current = child.head().unwrap().peel_to_commit().unwrap();
    child.branch("local", &current, false).unwrap();
    child.set_head("refs/heads/local").unwrap();
    let source = Repository::open(temporary.path().join("source-child")).unwrap();
    commit_file(&source, "file.txt", "remote update");
    let result = request(&parent, &scope, "pull_latest").unwrap();
    assert_eq!(result, json!({"completed": ["child"], "skipped": []}));
    assert_eq!(
        std::fs::read_to_string(parent.join("child/file.txt")).unwrap(),
        "remote update"
    );
}

fn delayed_pull_rejects_changed_settings(change_remote: bool) {
    use std::os::unix::fs::PermissionsExt;
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, parent, _, scope) = fixture(&["a", "b"]);
    for name in ["a", "b"] {
        let child = Repository::open(parent.join(name)).unwrap();
        let head = child.head().unwrap().peel_to_commit().unwrap();
        child.branch("local", &head, false).unwrap();
        child.set_head("refs/heads/local").unwrap();
    }
    let source = Repository::open(temporary.path().join("source-a")).unwrap();
    commit_file(&source, "file.txt", "new remote content");
    let hooks = temporary.path().join("pull-hooks");
    let started = temporary.path().join("pull-started");
    let release = temporary.path().join("pull-release");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-merge");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n: > {}\nwhile [ ! -e {} ]; do /bin/sleep 0.02; done\n",
            shell_path(&started),
            shell_path(&release)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    Repository::open(parent.join("a"))
        .unwrap()
        .config()
        .unwrap()
        .set_str("core.hooksPath", hooks.to_str().unwrap())
        .unwrap();
    std::thread::scope(|threads| {
        let release_on_drop = ReleaseOnDrop(release.clone());
        let pending = threads.spawn(|| request(&parent, &scope, "pull_latest"));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !started.exists() {
            assert!(
                Instant::now() < deadline,
                "first pull did not reach its hook"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let child = Repository::open(parent.join("b")).unwrap();
        if change_remote {
            child
                .remote_set_url("origin", "https://example.invalid/changed.git")
                .unwrap();
        } else {
            child
                .config()
                .unwrap()
                .set_str("pull.rebase", "true")
                .unwrap();
        }
        drop(release_on_drop);
        let problem = pending.join().unwrap().unwrap_err();
        assert_eq!(problem.code, "partial_failure", "{}", problem.message);
        assert!(problem.message.contains("remote"), "{}", problem.message);
        assert_eq!(
            std::fs::read_to_string(parent.join("b/file.txt")).unwrap(),
            "new"
        );
    });
}

#[test]
fn pull_latest_rejects_remote_or_pull_policy_changed_during_an_earlier_pull() {
    delayed_pull_rejects_changed_settings(true);
    delayed_pull_rejects_changed_settings(false);
}

#[test]
fn custom_update_is_rejected_before_git_runs() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, parent, _old, scope) = fixture(&["child"]);
    let marker = temporary.path().join("marker");
    Repository::open(&parent)
        .unwrap()
        .config()
        .unwrap()
        .set_str(
            "submodule.child.update",
            &format!("!touch {}", marker.display()),
        )
        .unwrap();
    let result = request(&parent, &scope, "update");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .message
            .contains("custom submodule update")
    );
    assert!(!marker.exists());
}

#[test]
fn direct_update_reports_partial_failure_and_keeps_remaining_dirty_worktree() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temporary, parent, old, scope) = fixture(&["a", "b"]);
    point_index_at(&parent, &["a", "b"], &old);
    std::fs::write(parent.join("b/file.txt"), "unsaved").unwrap();
    let result = request(&parent, &scope, "update").unwrap_err();
    assert_eq!(result.code, "partial_failure", "{}", result.message);
    assert!(result.message.contains("a"));
    assert_eq!(
        std::fs::read_to_string(parent.join("a/file.txt")).unwrap(),
        "old"
    );
    assert_eq!(
        std::fs::read_to_string(parent.join("b/file.txt")).unwrap(),
        "unsaved"
    );
}

#[test]
fn outside_symlink_and_stale_head_are_rejected_before_changes() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, parent, _old, scope) = fixture(&["child"]);
    let saved = temporary.path().join("outside");
    std::fs::rename(parent.join("child"), &saved).unwrap();
    std::os::unix::fs::symlink(&saved, parent.join("child")).unwrap();
    assert_eq!(
        request(&parent, &scope, "update").unwrap_err().code,
        "outside_workspace"
    );
    assert_eq!(
        std::fs::read_to_string(saved.join("file.txt")).unwrap(),
        "new"
    );
    let repo = Repository::open(&parent).unwrap();
    let head = repo.head().unwrap();
    let stale = execute(
        &scope,
        Request {
            repo: parent,
            operation: "sync".into(),
            expected_branch: Some(head.shorthand().unwrap().to_owned()),
            expected_head: "0".repeat(40),
        },
    )
    .unwrap_err();
    assert_eq!(stale.code, "stale_target");
}
