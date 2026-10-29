use std::io::Cursor;
use std::path::Path;

use git2::{Repository, Signature};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::serve;
use crate::config::SubmoduleConfig;

fn repo_with_files(path: &Path) -> Repository {
    std::fs::create_dir_all(path).unwrap();
    let repo = Repository::init(path).unwrap();
    let mut index = repo.index().unwrap();
    for name in ["file.txt", "sentinel.txt"] {
        std::fs::write(path.join(name), "original\n").unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

fn mutation(workspace: &Path, method: &str, params: Value) -> Value {
    let input = format!(
        "{}\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": method, "params": params})
    );
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        None,
        true,
    )
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies[0]["result"]["version"], 1);
    replies[1].clone()
}

fn preview(workspace: &Path, params: Value) -> Value {
    let input = format!(
        "{}\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "file_action_preview", "params": params})
    );
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        None,
        false,
    )
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies[0]["result"]["version"], 1);
    replies[1].clone()
}

fn confirmed_mutation(workspace: &Path, method: &str, mut params: Value) -> Value {
    if method == "file_action"
        && matches!(
            params["operation"].as_str(),
            Some("discard" | "delete_untracked")
        )
    {
        let result = preview(workspace, params.clone());
        if result.get("error").is_some() {
            return result;
        }
        params["expected_token"] = result["result"]["token"].clone();
    }
    mutation(workspace, method, params)
}

#[test]
fn mutation_worker_rejects_read_requests_and_exits_before_later_mutations() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let _repo = repo_with_files(&repo_path);
    std::fs::write(repo_path.join("untracked"), "keep").unwrap();
    let input = format!(
        "{}\n{}\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "status", "params": {"repo": repo_path}}),
        json!({"id": 3, "method": "file_action", "params": {"repo": repo_path, "path": "untracked", "operation": "delete_untracked"}})
    );
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        None,
        true,
    )
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["error"]["code"], "invalid_request");
    assert!(repo_path.join("untracked").exists());
}

#[test]
fn malformed_mutation_request_terminates_worker_before_later_actions() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let _repo = repo_with_files(&repo_path);
    std::fs::write(repo_path.join("untracked"), "keep").unwrap();
    let input = format!(
        "{}\ninvalid json\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 3, "method": "file_action", "params": {"repo": repo_path, "path": "untracked", "operation": "delete_untracked"}})
    );
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        None,
        true,
    )
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["error"]["code"], "invalid_request");
    assert!(repo_path.join("untracked").exists());
}

#[test]
fn stages_unstages_and_discards_selected_file_only() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = repo_with_files(&repo_path);
    std::fs::write(repo_path.join("file.txt"), "changed\n").unwrap();
    std::fs::write(repo_path.join("sentinel.txt"), "sentinel changed\n").unwrap();
    let action = |operation| {
        confirmed_mutation(
            temporary.path(),
            "file_action",
            json!({"repo": repo_path, "path": "file.txt", "operation": operation}),
        )
    };
    assert_eq!(action("stage")["result"], json!({}));
    assert!(
        repo.status_file(Path::new("file.txt"))
            .unwrap()
            .is_index_modified()
    );
    assert!(
        !repo
            .status_file(Path::new("sentinel.txt"))
            .unwrap()
            .is_index_modified()
    );
    assert_eq!(action("unstage")["result"], json!({}));
    assert!(
        repo.status_file(Path::new("file.txt"))
            .unwrap()
            .is_wt_modified()
    );
    assert_eq!(action("discard")["result"], json!({}));
    assert_eq!(
        std::fs::read_to_string(repo_path.join("file.txt")).unwrap(),
        "original\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("sentinel.txt")).unwrap(),
        "sentinel changed\n"
    );
}

#[cfg(unix)]
#[test]
fn literal_untracked_filename_is_staged_and_deleted_without_touching_neighbors() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = repo_with_files(&repo_path);
    std::fs::write(repo_path.join("*.txt"), "selected").unwrap();
    let params = |operation| json!({"repo": repo_path, "path": "*.txt", "operation": operation});
    assert_eq!(
        mutation(temporary.path(), "file_action", params("stage"))["result"],
        json!({})
    );
    assert!(repo.status_file(Path::new("*.txt")).unwrap().is_index_new());
    assert_eq!(
        mutation(temporary.path(), "file_action", params("unstage"))["result"],
        json!({})
    );
    assert_eq!(
        confirmed_mutation(temporary.path(), "file_action", params("delete_untracked"))["result"],
        json!({})
    );
    assert!(!repo_path.join("*.txt").exists());
    assert!(repo_path.join("file.txt").exists());
}

#[test]
fn worktree_actions_create_inside_workspace_and_refuse_dirty_removal() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let _repo = repo_with_files(&repo_path);
    let worktree = workspace.join("worktree");
    let create = |path: &Path, branch: &str| {
        mutation(
            &workspace,
            "worktree_create",
            json!({"repo": repo_path, "path": path, "branch": branch}),
        )
    };
    assert_eq!(
        create(&temporary.path().join("outside"), "outside")["error"]["code"],
        "outside_workspace"
    );
    assert_eq!(
        create(&worktree, "-bad")["error"]["code"],
        "invalid_request"
    );
    assert_eq!(create(&worktree, "feature")["result"], json!({}));
    let remove = |path: &Path| {
        mutation(
            &workspace,
            "worktree_remove",
            json!({"repo": repo_path, "path": path}),
        )
    };
    assert_eq!(remove(&repo_path)["error"]["code"], "invalid_request");
    std::fs::write(worktree.join("untracked"), "dirty").unwrap();
    assert_eq!(remove(&worktree)["error"]["code"], "git_error");
    assert!(worktree.exists());
    std::fs::remove_file(worktree.join("untracked")).unwrap();
    assert_eq!(remove(&worktree)["result"], json!({}));
    assert!(!worktree.exists());
}

#[cfg(unix)]
#[test]
fn actions_reject_traversal_and_symlink_escape() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let _repo = repo_with_files(&repo_path);
    let outside = temporary.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), "preserve").unwrap();
    std::os::unix::fs::symlink(&outside, repo_path.join("link")).unwrap();
    for path in ["../outside/file", "link/file"] {
        let reply = mutation(
            &workspace,
            "file_action",
            json!({"repo": repo_path, "path": path, "operation": "delete_untracked"}),
        );
        assert!(reply.get("error").is_some(), "{reply}");
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("file")).unwrap(),
        "preserve"
    );
    let destination = repo_path.join("link/new");
    let reply = mutation(
        &workspace,
        "worktree_create",
        json!({"repo": repo_path, "path": destination, "branch": "escape"}),
    );
    assert_eq!(reply["error"]["code"], "outside_workspace");
    assert!(!outside.join("new").exists());
}

#[cfg(unix)]
#[test]
fn file_actions_operate_on_external_and_dangling_symlink_entries() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = repo_with_files(&repo_path);
    let outside = temporary.path().join("outside");
    std::fs::write(&outside, "preserve").unwrap();
    let dangling = temporary.path().join("missing");
    for (name, target) in [("external-link", &outside), ("dangling-link", &dangling)] {
        std::os::unix::fs::symlink(target, repo_path.join(name)).unwrap();
        let params = |operation| json!({"repo": repo_path, "path": name, "operation": operation});
        assert_eq!(
            mutation(temporary.path(), "file_action", params("stage"))["result"],
            json!({})
        );
        assert!(repo.status_file(Path::new(name)).unwrap().is_index_new());
        assert_eq!(
            mutation(temporary.path(), "file_action", params("unstage"))["result"],
            json!({})
        );
        assert_eq!(
            confirmed_mutation(temporary.path(), "file_action", params("delete_untracked"))["result"],
            json!({})
        );
        assert!(repo_path.join(name).symlink_metadata().is_err());
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve");
    assert!(!dangling.exists());
}

#[test]
fn destructive_action_rejects_missing_token_and_changes_after_preview() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let _repo = repo_with_files(&repo_path);
    std::fs::write(repo_path.join("file.txt"), "dirty one").unwrap();
    let params = json!({"repo": repo_path, "path": "file.txt", "operation": "discard"});
    let token = preview(temporary.path(), params.clone())["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        mutation(temporary.path(), "file_action", params.clone())["error"]["code"],
        "invalid_request"
    );
    let mut wrong = params.clone();
    wrong["expected_token"] = json!("a".repeat(64));
    assert_eq!(
        mutation(temporary.path(), "file_action", wrong)["error"]["code"],
        "file_changed"
    );
    std::fs::write(repo_path.join("file.txt"), "dirty two").unwrap();
    let mut stale = params.clone();
    stale["expected_token"] = json!(token);
    assert_eq!(
        mutation(temporary.path(), "file_action", stale)["error"]["code"],
        "file_changed"
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("file.txt")).unwrap(),
        "dirty two"
    );
    assert_eq!(
        confirmed_mutation(temporary.path(), "file_action", params)["result"],
        json!({})
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("file.txt")).unwrap(),
        "original\n"
    );

    std::fs::write(repo_path.join("new.txt"), "first").unwrap();
    let params = json!({"repo": repo_path, "path": "new.txt", "operation": "delete_untracked"});
    let token = preview(temporary.path(), params.clone())["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(repo_path.join("new.txt"), "later").unwrap();
    let mut stale = params;
    stale["expected_token"] = json!(token);
    assert_eq!(
        mutation(temporary.path(), "file_action", stale)["error"]["code"],
        "file_changed"
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("new.txt")).unwrap(),
        "later"
    );
}

#[cfg(unix)]
#[test]
fn changed_symlink_target_cannot_be_deleted_using_prior_confirmation() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let _repo = repo_with_files(&repo_path);
    let link = repo_path.join("link");
    std::os::unix::fs::symlink("missing-one", &link).unwrap();
    let params = json!({"repo": repo_path, "path": "link", "operation": "delete_untracked"});
    let token = preview(temporary.path(), params.clone())["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("missing-two", &link).unwrap();
    let mut stale = params;
    stale["expected_token"] = json!(token);
    assert_eq!(
        mutation(temporary.path(), "file_action", stale)["error"]["code"],
        "file_changed"
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("missing-two"));
}

#[test]
fn staged_rename_cannot_discard_a_recreated_original_after_preview() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let _repo = repo_with_files(&repo_path);
    let output = crate::git::process::git_command(&repo_path)
        .args(["mv", "--", "file.txt", "renamed.txt"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let params = json!({"repo": repo_path, "path": "renamed.txt", "operation": "discard"});
    let token = preview(temporary.path(), params.clone())["result"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(repo_path.join("file.txt"), "new original").unwrap();
    let mut stale = params;
    stale["expected_token"] = json!(token);
    let result = mutation(temporary.path(), "file_action", stale);
    assert!(result.get("error").is_some(), "{result}");
    assert_eq!(
        std::fs::read_to_string(repo_path.join("file.txt")).unwrap(),
        "new original"
    );
    assert!(repo_path.join("renamed.txt").exists());
}

fn commit_file(repo: &Repository, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.workdir().unwrap().join(name), contents).unwrap();
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &[&parent],
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn worktree_creation_disables_configured_submodule_recursion() {
    use std::os::unix::fs::PermissionsExt;

    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = repo_with_files(&repo_path);
    let source = temporary.path().join("source");
    let _source = repo_with_files(&source);
    let added = crate::git::process::git_command(&repo_path)
        .args(["-c", "protocol.file.allow=always", "submodule", "add"])
        .arg(&source)
        .arg("modules/child")
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    commit_file(&repo, "extra.txt", "tracked\n", "add submodule");
    repo.config()
        .unwrap()
        .set_bool("submodule.recurse", true)
        .unwrap();
    let hook = repo.path().join("hooks/post-checkout");
    std::fs::write(
        &hook,
        "#!/bin/sh\ngit config --bool submodule.recurse > \"$PWD/recurse-value\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let destination = workspace.join("new-worktree");
    let result = mutation(
        &workspace,
        "worktree_create",
        json!({
            "repo": repo_path, "path": destination, "branch": "feature"
        }),
    );
    assert_eq!(result["result"], json!({}), "{result}");
    assert_eq!(
        std::fs::read_to_string(destination.join("recurse-value"))
            .unwrap()
            .trim(),
        "false"
    );
    assert!(!destination.join("modules/child/.git").exists());
    assert!(destination.join("file.txt").exists());
}

#[test]
fn repo_sync_pushes_pulls_and_rebases_using_configured_remote() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = repo_with_files(&repo_path);
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    let remote_path = temporary.path().join("remote.git");
    let remote = Repository::init_bare(&remote_path).unwrap();
    repo.remote("gerrit", remote_path.to_str().unwrap())
        .unwrap();
    let sync = |operation| {
        mutation(
            &workspace,
            "repo_sync",
            json!({"repo": repo_path, "operation": operation, "expected_branch": branch, "expected_head": repo.head().unwrap().target().unwrap().to_string()}),
        )
    };
    assert_eq!(sync("push")["result"], json!({}));
    let refname = format!("refs/heads/{branch}");
    assert_eq!(
        remote.find_reference(&refname).unwrap().target(),
        repo.head().unwrap().target()
    );
    remote.set_head(&refname).unwrap();
    {
        let mut config = repo.config().unwrap();
        config
            .set_str(&format!("branch.{branch}.remote"), "gerrit")
            .unwrap();
        config
            .set_str(&format!("branch.{branch}.merge"), &refname)
            .unwrap();
        config.set_str("user.name", "Test").unwrap();
        config
            .set_str("user.email", "test@example.invalid")
            .unwrap();
        config.set_bool("submodule.recurse", true).unwrap();
        config.set_bool("fetch.recurseSubmodules", true).unwrap();
        config
            .set_str("push.recurseSubmodules", "on-demand")
            .unwrap();
    }
    let peer_path = temporary.path().join("peer");
    let peer = Repository::clone(remote_path.to_str().unwrap(), &peer_path).unwrap();
    let push_peer = || {
        let output = crate::git::process::git_command(&peer_path)
            .args(["push", "origin", &format!("HEAD:{refname}")])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    commit_file(&peer, "remote.txt", "remote\n", "remote change");
    push_peer();
    assert_eq!(sync("pull")["result"], json!({}));
    assert_eq!(
        std::fs::read_to_string(repo_path.join("remote.txt")).unwrap(),
        "remote\n"
    );
    commit_file(&repo, "local.txt", "local\n", "local change");
    commit_file(&peer, "other.txt", "other\n", "other change");
    push_peer();
    let rebase = sync("pull_rebase");
    assert_eq!(rebase["result"], json!({}), "{rebase}");
    assert_eq!(
        std::fs::read_to_string(repo_path.join("local.txt")).unwrap(),
        "local\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo_path.join("other.txt")).unwrap(),
        "other\n"
    );
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .summary()
            .unwrap(),
        Some("local change")
    );
}

#[test]
fn repo_sync_rejects_outside_scope_and_unknown_actions() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = repo_with_files(&repo_path);
    let outside = temporary.path().join("outside");
    let outside_repo = repo_with_files(&outside);
    let original = outside_repo.head().unwrap().target();
    let denied = mutation(
        &workspace,
        "repo_sync",
        json!({"repo": outside, "operation": "push", "expected_branch": null, "expected_head": "0000000000000000000000000000000000000000"}),
    );
    assert_eq!(denied["error"]["code"], "outside_workspace");
    let invalid = mutation(
        &workspace,
        "repo_sync",
        json!({"repo": repo_path, "operation": "pull_force", "expected_branch": repo.head().unwrap().shorthand().unwrap(), "expected_head": repo.head().unwrap().target().unwrap().to_string()}),
    );
    assert_eq!(invalid["error"]["code"], "invalid_request");
    assert_eq!(outside_repo.head().unwrap().target(), original);
}

#[test]
fn repo_sync_rejects_changed_branch_or_commit_before_running_git() {
    if !crate::git::git_test_available() {
        return;
    }
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = repo_with_files(&repo_path);
    let initial = repo.head().unwrap().peel_to_commit().unwrap();
    let initial_oid = initial.id().to_string();
    let original_branch = repo.head().unwrap().shorthand().unwrap().to_owned();
    repo.branch("other", &initial, false).unwrap();
    repo.set_head("refs/heads/other").unwrap();
    let stale = || {
        mutation(
            temporary.path(),
            "repo_sync",
            json!({"repo": repo_path, "operation": "push", "expected_branch": original_branch, "expected_head": initial_oid}),
        )
    };
    assert_eq!(stale()["error"]["code"], "stale_target");
    repo.set_head(&format!("refs/heads/{original_branch}"))
        .unwrap();
    commit_file(&repo, "new.txt", "new", "new commit");
    assert_eq!(stale()["error"]["code"], "stale_target");
    let current = repo.head().unwrap().target().unwrap();
    repo.set_head_detached(current).unwrap();
    let detached_stale = mutation(
        temporary.path(),
        "repo_sync",
        json!({"repo": repo_path, "operation": "push", "expected_branch": original_branch, "expected_head": current.to_string()}),
    );
    assert_eq!(detached_stale["error"]["code"], "stale_target");
    let detached_current = mutation(
        temporary.path(),
        "repo_sync",
        json!({"repo": repo_path, "operation": "push", "expected_branch": null, "expected_head": current.to_string()}),
    );
    assert_eq!(detached_current["error"]["code"], "git_error");
}
