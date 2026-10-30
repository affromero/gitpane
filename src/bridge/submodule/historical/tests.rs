use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use super::{ContentRequest, content};
use crate::bridge::{changes::RepositoryIdentity, scope::Scope};

fn fixture(entries: &[(&str, &[u8], i32)]) -> (TempDir, PathBuf, Oid, Vec<Oid>) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    let mut ids = Vec::new();
    for (name, bytes, mode) in entries {
        let oid = repo.blob(bytes).unwrap();
        builder.insert(*name, oid, *mode).unwrap();
        ids.push(oid);
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    drop(builder);
    drop(repo);
    (directory, root, commit, ids)
}

fn request(repo: &Path, commit: Oid, path: &str, oid: Oid, mode: i32) -> ContentRequest {
    ContentRequest {
        repo: repo.to_path_buf(),
        expected_repo_identity: RepositoryIdentity::read(repo)
            .unwrap()
            .fingerprint()
            .unwrap(),
        commit_oid: commit.to_string(),
        path: PathBuf::from(path),
        expected_oid: oid.to_string(),
        expected_mode: mode,
    }
}

#[test]
fn reports_file_and_symlink_sizes_from_the_selected_repository_tree() {
    let (_directory, root, commit, ids) = fixture(&[
        ("file", b"file contents\n", 0o100644),
        ("link", b"../other", 0o120000),
    ]);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    assert_eq!(
        content(&scope, request(&root, commit, "file", ids[0], 0o100644)).unwrap()["size"],
        14
    );
    assert_eq!(
        content(&scope, request(&root, commit, "link", ids[1], 0o120000)).unwrap()["size"],
        8
    );
}

#[test]
fn rejects_mismatched_tree_endpoints_and_repository_identity() {
    let (_directory, root, commit, ids) =
        fixture(&[("file", b"one", 0o100644), ("other", b"two", 0o100644)]);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    for request in [
        request(&root, commit, "file", ids[1], 0o100644),
        request(&root, commit, "file", ids[0], 0o100755),
        request(&root, commit, "missing", ids[0], 0o100644),
        request(&root, ids[0], "file", ids[0], 0o100644),
    ] {
        assert_eq!(content(&scope, request).unwrap_err().code, "stale_target");
    }
    let mut changed = request(&root, commit, "file", ids[0], 0o100644);
    changed.expected_repo_identity = "1".repeat(64);
    assert_eq!(content(&scope, changed).unwrap_err().code, "stale_target");
}

#[test]
fn rejects_unsafe_paths_modes_and_incomplete_object_ids() {
    let (_directory, root, commit, ids) = fixture(&[("file", b"content", 0o100644)]);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    for path in ["", "../file", "/file", "file\0name"] {
        assert_eq!(
            content(&scope, request(&root, commit, path, ids[0], 0o100644))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    let mut bad_mode = request(&root, commit, "file", ids[0], 0o160000);
    assert_eq!(
        content(&scope, bad_mode).unwrap_err().code,
        "invalid_request"
    );
    bad_mode = request(&root, commit, "file", ids[0], 0o100644);
    bad_mode.commit_oid = "abc".to_owned();
    assert_eq!(
        content(&scope, bad_mode).unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn reports_sizes_for_binary_non_utf8_and_large_objects() {
    let huge = vec![b'a'; 1024 * 1024 + 1];
    let (_directory, root, commit, ids) = fixture(&[
        ("nul", b"a\0b", 0o100644),
        ("invalid", b"\xff", 0o100644),
        ("huge", &huge, 0o100644),
    ]);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    for (path, oid, expected_size) in [
        ("nul", ids[0], 3),
        ("invalid", ids[1], 1),
        ("huge", ids[2], 1024 * 1024 + 1),
    ] {
        assert_eq!(
            content(&scope, request(&root, commit, path, oid, 0o100644)).unwrap()["size"],
            expected_size
        );
    }
}

#[test]
fn rejects_repositories_outside_the_workspace() {
    let (_directory, root, commit, ids) = fixture(&[("file", b"content", 0o100644)]);
    let unrelated = tempfile::tempdir().unwrap();
    let scope = Scope::new(vec![unrelated.path().canonicalize().unwrap()]).unwrap();
    assert_eq!(
        content(&scope, request(&root, commit, "file", ids[0], 0o100644))
            .unwrap_err()
            .code,
        "outside_workspace"
    );
}
