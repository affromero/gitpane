#![cfg(unix)]

use super::*;
use git2::{Repository, Signature};
use std::fs;

fn fixture(submodule_path: &str) -> (tempfile::TempDir, PathBuf, Repository, Scope) {
    let temp = tempfile::tempdir().unwrap();
    let parent_path = temp.path().join("parent");
    let source_path = temp.path().join("source");
    fs::create_dir(&parent_path).unwrap();
    fs::create_dir(&source_path).unwrap();
    let source = Repository::init(&source_path).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let source_tree = source
        .find_tree(source.index().unwrap().write_tree().unwrap())
        .unwrap();
    source
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "source",
            &source_tree,
            &[],
        )
        .unwrap();
    let parent = Repository::init(&parent_path).unwrap();
    let initial_tree = parent
        .find_tree(parent.index().unwrap().write_tree().unwrap())
        .unwrap();
    parent
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "initial",
            &initial_tree,
            &[],
        )
        .unwrap();
    let mut module = parent
        .submodule(
            source_path.to_str().unwrap(),
            Path::new(submodule_path),
            true,
        )
        .unwrap();
    module.clone(None).unwrap();
    module.add_finalize().unwrap();
    drop(module);
    let tree = parent
        .find_tree(parent.index().unwrap().write_tree().unwrap())
        .unwrap();
    let previous = parent.head().unwrap().peel_to_commit().unwrap();
    parent
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "submodule",
            &tree,
            &[&previous],
        )
        .unwrap();
    drop(source_tree);
    drop(source);
    drop((initial_tree, tree, previous));
    let parent_path = parent_path.canonicalize().unwrap();
    let scope = Scope::new(vec![parent_path.clone()]).unwrap();
    (temp, parent_path, parent, scope)
}

fn selected(repo: &Repository, root: &Path, path: &str) -> CompareRequest {
    let record = changes::current_record(repo, &SubmoduleConfig::default(), path, "working")
        .unwrap()
        .unwrap();
    assert_eq!(record["is_submodule"], true);
    CompareRequest {
        repo: root.to_owned(),
        record,
        expected_repo_identity: RepositoryIdentity::read(root)
            .unwrap()
            .fingerprint()
            .unwrap(),
    }
}

fn compare_selected(scope: &Scope, request: &CompareRequest) -> Value {
    compare(
        scope,
        CompareRequest {
            repo: request.repo.clone(),
            record: request.record.clone(),
            expected_repo_identity: request.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap()
}

fn content_side(
    scope: &Scope,
    request: &CompareRequest,
    snapshot: &Value,
    side: &str,
) -> Result<Value, BridgeError> {
    content(
        scope,
        ContentRequest {
            repo: request.repo.clone(),
            record: request.record.clone(),
            expected_repo_identity: request.expected_repo_identity.clone(),
            head_oid: snapshot["head_oid"].as_str().map(str::to_owned),
            sides: snapshot["sides"].clone(),
            side: side.to_owned(),
        },
        &SubmoduleConfig::default(),
    )
}

fn assert_changes_page(root: &Path, path: &str, kind: &str) {
    let config = crate::config::Config::default();
    let options = crate::bridge::BridgeOptions {
        submodules: &config.submodules,
        graph: &config.graph,
        worktree: &config.worktree,
        review_base: None,
        theme: &config,
        monitor_stdio: false,
    };
    let mut scope = Some(Scope::new(vec![root.to_owned()]).unwrap());
    let mut snapshots = changes::Snapshots::default();
    let request =
        serde_json::from_value(json!({"id":1,"method":"changes_begin","params":{"repo":root}}))
            .unwrap();
    let page = crate::bridge::process(&mut scope, &mut snapshots, request, &options).unwrap();
    assert!(
        page["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["path"] == path
                && record["kind"] == kind
                && record["is_submodule"] == true)
    );
}

#[test]
fn replaced_submodule_compares_index_pointer_with_verified_file() {
    let (_temp, root, repo, scope) = fixture("sub");
    fs::remove_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub"), b"first replacement\n").unwrap();
    let status = crate::bridge::status::query(
        &root,
        &scope,
        &SubmoduleConfig::default(),
        false,
        false,
        &[],
    )
    .unwrap();
    assert!(
        status["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "sub")
    );
    assert_changes_page(&root, "sub", "type_changed");
    let request = selected(&repo, &root, "sub");
    assert_eq!(request.record["kind"], "type_changed");
    let snapshot = compare_selected(&scope, &request);
    assert_eq!(snapshot["sides"]["index"]["kind"], "gitlink");
    assert_eq!(snapshot["sides"]["working"]["kind"], "working_file");
    assert!(
        content_side(&scope, &request, &snapshot, "index").unwrap()["content"]
            .as_str()
            .unwrap()
            .starts_with("Subproject commit ")
    );
    assert_eq!(
        content_side(&scope, &request, &snapshot, "working").unwrap()["content"],
        "first replacement\n"
    );
    fs::write(root.join("sub"), b"other replacement\n").unwrap();
    assert_eq!(
        content_side(&scope, &request, &snapshot, "working")
            .unwrap_err()
            .code,
        "stale_target"
    );
}

#[test]
fn deleted_nested_submodule_remains_comparable_when_ancestor_is_file() {
    let (_temp, root, repo, scope) = fixture("dir/sub");
    fs::remove_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir"), b"replacement").unwrap();
    let status = crate::bridge::status::query(
        &root,
        &scope,
        &SubmoduleConfig::default(),
        false,
        false,
        &[],
    )
    .unwrap();
    assert!(
        status["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "dir/sub")
    );
    assert_changes_page(&root, "dir/sub", "working_deleted");
    let request = selected(&repo, &root, "dir/sub");
    assert_eq!(request.record["kind"], "working_deleted");
    let snapshot = compare_selected(&scope, &request);
    assert_eq!(snapshot["sides"]["index"]["kind"], "gitlink");
    assert!(snapshot["sides"]["working"].is_null());
    assert_eq!(
        content_side(&scope, &request, &snapshot, "working").unwrap()["content"],
        ""
    );
}

#[test]
fn working_file_comparison_refuses_binary_and_oversized_contents() {
    let (_temp, root, repo, scope) = fixture("sub");
    fs::remove_dir_all(root.join("sub")).unwrap();
    for bytes in [b"binary\0data".to_vec(), vec![b'x'; MAX_CONTENT + 1]] {
        fs::write(root.join("sub"), bytes).unwrap();
        let request = selected(&repo, &root, "sub");
        let error = compare(&scope, request, &SubmoduleConfig::default()).unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }
}

#[test]
fn changed_index_and_outside_leaf_cannot_reuse_a_working_snapshot() {
    use std::os::unix::fs::symlink;

    let (temp, root, repo, scope) = fixture("sub");
    fs::remove_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub"), b"replacement\n").unwrap();
    let request = selected(&repo, &root, "sub");
    let snapshot = compare_selected(&scope, &request);
    let mut index = repo.index().unwrap();
    let mut entry = index.get_path(Path::new("sub"), 0).unwrap();
    entry.id = git2::Oid::from_str(&"1".repeat(40)).unwrap();
    index.add(&entry).unwrap();
    index.write().unwrap();
    assert_eq!(
        content_side(&scope, &request, &snapshot, "index")
            .unwrap_err()
            .code,
        "stale_target"
    );

    let request = selected(&repo, &root, "sub");
    let snapshot = compare_selected(&scope, &request);
    let outside = temp.path().join("outside");
    fs::write(&outside, b"outside secret\n").unwrap();
    fs::remove_file(root.join("sub")).unwrap();
    symlink(&outside, root.join("sub")).unwrap();
    assert_eq!(
        content_side(&scope, &request, &snapshot, "working")
            .unwrap_err()
            .code,
        "outside_workspace"
    );
}

#[test]
fn outside_symlink_blocking_nested_submodule_is_not_accepted() {
    use std::os::unix::fs::symlink;

    let (temp, root, _repo, scope) = fixture("dir/sub");
    fs::remove_dir_all(root.join("dir")).unwrap();
    let outside = temp.path().join("outside");
    fs::write(&outside, b"outside secret").unwrap();
    symlink(&outside, root.join("dir")).unwrap();
    assert!(scope.submodule_repositories(&root).is_err());
}

#[test]
fn replacing_a_blocking_file_at_the_same_path_changes_its_identity() {
    let (temp, root, _repo, _scope) = fixture("dir/sub");
    fs::remove_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir"), b"first").unwrap();
    let first = crate::git::status::regular_submodule_blocker(&root.join("dir/sub"))
        .unwrap()
        .unwrap();
    let replacement = temp.path().join("replacement");
    fs::write(&replacement, b"other").unwrap();
    fs::rename(replacement, root.join("dir")).unwrap();
    let second = crate::git::status::regular_submodule_blocker(&root.join("dir/sub"))
        .unwrap()
        .unwrap();
    assert_eq!(first.path, second.path);
    assert_ne!(first, second);
}
