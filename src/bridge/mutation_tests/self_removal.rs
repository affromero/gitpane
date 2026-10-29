use super::*;

fn git(repo: &Path, args: &[&str]) {
    let output = crate::git::process::git_command(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn params(path: &Path, preview: &Value) -> Value {
    json!({"repo": path, "path": path,
        "expected_repo_identity": preview["result"]["repo_identity"],
        "expected_worktree_identity": preview["result"]["worktree_identity"]})
}

fn status(workspace: &Path, repo: &Path) -> Value {
    let input = format!(
        "{}\n{}\n",
        json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[workspace]}}),
        json!({"id":2,"method":"status","params":{"repo":repo}})
    );
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        &GraphConfig::default(),
        &WorktreeConfig::default(),
        None,
        false,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .nth(1)
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap()
}

#[test]
fn lone_linked_root_can_be_removed_only_with_current_identity_and_git_safety() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let main = temp.path().join("main repo");
    drop(repo_with_files(&main));
    let linked = temp.path().join("linked repo");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    let linked_status = status(&linked, &linked);
    assert_eq!(
        linked_status["result"]["is_linked_worktree"], true,
        "{linked_status:?}"
    );
    let preview = worktree_preview(&linked, &linked, &linked);
    assert_eq!(preview["result"]["self_removal"], true);
    assert_eq!(
        preview["result"]["repo_identity"],
        linked_status["result"]["repo_identity"]
    );
    assert_eq!(
        preview["result"]["repo_identity"],
        preview["result"]["worktree_identity"]
    );
    assert_eq!(
        mutation(
            &linked,
            "worktree_remove",
            json!({"repo": linked, "path": linked})
        )["error"]["code"],
        "invalid_request"
    );
    let mut bad = params(&linked, &preview);
    bad["expected_repo_identity"] = json!("x".repeat(64));
    assert_eq!(
        mutation(&linked, "worktree_remove", bad)["error"]["code"],
        "invalid_request"
    );
    std::fs::write(linked.join("dirty"), "dirty").unwrap();
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &preview))["error"]["code"],
        "git_error"
    );
    std::fs::remove_file(linked.join("dirty")).unwrap();
    git(&main, &["worktree", "lock", linked.to_str().unwrap()]);
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &preview))["error"]["code"],
        "git_error"
    );
    git(&main, &["worktree", "unlock", linked.to_str().unwrap()]);
    let gitdir = Repository::open(&linked)
        .unwrap()
        .path()
        .canonicalize()
        .unwrap();
    let backup = temp.path().join("backup");
    std::fs::rename(&gitdir, &backup).unwrap();
    copy_directory(&backup, &gitdir);
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &preview))["error"]["code"],
        "stale_target"
    );
    assert!(linked.exists());
    let fresh = worktree_preview(&linked, &linked, &linked);
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &fresh))["result"],
        json!({})
    );
    assert!(!linked.exists());
    assert!(main.exists());
    assert_eq!(
        worktree_preview(&main, &main, &main)["error"]["code"],
        "invalid_request"
    );
    assert_eq!(status(&main, &main)["result"]["is_linked_worktree"], false);
}

#[test]
fn self_removal_supports_bare_and_separate_git_directories() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    drop(repo_with_files(&source));
    let bare = temp.path().join("bare.git");
    git(
        temp.path(),
        &[
            "clone",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let linked = temp.path().join("bare linked");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-b",
            "bare-branch",
            linked.to_str().unwrap(),
        ],
    );
    let preview = worktree_preview(&linked, &linked, &linked);
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &preview))["result"],
        json!({})
    );
    assert!(bare.exists());

    let main = temp.path().join("separate main");
    let separate = temp.path().join("separate git directory");
    git(
        temp.path(),
        &[
            "clone",
            "--separate-git-dir",
            separate.to_str().unwrap(),
            source.to_str().unwrap(),
            main.to_str().unwrap(),
        ],
    );
    let linked = temp.path().join("separate linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "separate-branch",
            linked.to_str().unwrap(),
        ],
    );
    let preview = worktree_preview(&linked, &linked, &linked);
    assert_eq!(
        mutation(&linked, "worktree_remove", params(&linked, &preview))["result"],
        json!({})
    );
    assert!(main.exists());
    assert!(separate.exists());
}

#[cfg(unix)]
#[test]
fn self_removal_rejects_retargeted_workspace_source() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let main = temp.path().join("main");
    drop(repo_with_files(&main));
    let linked = temp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    let other = temp.path().join("other");
    drop(repo_with_files(&other));
    let source = temp.path().join("workspace");
    std::os::unix::fs::symlink(&linked, &source).unwrap();
    let scope = super::super::scope::Scope::new(vec![source.clone()]).unwrap();
    let preview = super::super::mutation::worktree_remove_preview(
        &scope,
        serde_json::from_value(json!({"repo": source, "path": source})).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(&other, &source).unwrap();
    let request = json!({"repo": source, "path": source,
        "expected_repo_identity": preview["repo_identity"],
        "expected_worktree_identity": preview["worktree_identity"]});
    let error =
        super::super::mutation::worktree_remove(&scope, serde_json::from_value(request).unwrap())
            .unwrap_err();
    assert_eq!(error.code, "outside_workspace");
    assert!(linked.exists());
    assert!(other.exists());
}

#[test]
fn removing_worktree_refuses_nested_workspace_even_when_git_ignores_it() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let main = temp.path().join("main");
    drop(repo_with_files(&main));
    let linked = temp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    std::fs::write(main.join(".git/info/exclude"), "nested/\n").unwrap();
    let nested = linked.join("nested");
    drop(repo_with_files(&nested));
    std::fs::write(nested.join("unique"), "must survive").unwrap();
    let prior_scope = super::super::scope::Scope::new(vec![main.clone(), linked.clone()]).unwrap();
    let scope = super::super::scope::Scope::new(vec![main.clone(), linked.clone(), nested.clone()])
        .unwrap();
    for anchor in [&main, &linked] {
        let preview = super::super::mutation::worktree_remove_preview(
            &prior_scope,
            serde_json::from_value(json!({"repo": anchor, "path": linked})).unwrap(),
        )
        .unwrap();
        let request = json!({"repo": anchor, "path": linked});
        let error = super::super::mutation::worktree_remove_preview(
            &scope,
            serde_json::from_value(request).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("workspace folder"));
        let request = json!({"repo": anchor, "path": linked,
            "expected_repo_identity": preview["repo_identity"],
            "expected_worktree_identity": preview["worktree_identity"]});
        let error = super::super::mutation::worktree_remove(
            &scope,
            serde_json::from_value(request).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }
    assert_eq!(
        std::fs::read_to_string(nested.join("unique")).unwrap(),
        "must survive"
    );
}

#[cfg(unix)]
#[test]
fn removing_worktree_refuses_nested_workspace_alias_to_external_repo() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = TempDir::new().unwrap();
    let main = temp.path().join("main");
    drop(repo_with_files(&main));
    let linked = temp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    let outside = temp.path().join("outside");
    drop(repo_with_files(&outside));
    let alias = linked.join("alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    let scope = super::super::scope::Scope::new(vec![linked.clone(), alias]).unwrap();
    let request = json!({"repo": linked, "path": linked});
    let error = super::super::mutation::worktree_remove_preview(
        &scope,
        serde_json::from_value(request).unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert!(error.message.contains("workspace folder"));
    assert!(linked.exists());
}
