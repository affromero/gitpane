mod diagnostic_tests;

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

use crate::bridge::scope::Scope;
use crate::config::SubmoduleConfig;
use crate::config::{GraphConfig, WorktreeConfig};

use super::{Begin, FindRequest, PageRequest, Release, Snapshots};

fn setup() -> (TempDir, PathBuf, Repository) {
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("repo");
    fs::create_dir(&path).unwrap();
    let repo = Repository::init(&path).unwrap();
    for (name, value) in [
        ("mixed.txt", "initial\n"),
        ("old.txt", "rename content\n"),
        ("delete.txt", "delete me\n"),
        (".gitignore", "ignored.txt\n"),
    ] {
        fs::write(path.join(name), value).unwrap();
    }
    let mut index = repo.index().unwrap();
    for name in ["mixed.txt", "old.txt", "delete.txt", ".gitignore"] {
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    (temporary, path.canonicalize().unwrap(), repo)
}

fn begin(store: &mut Snapshots, scope: &Scope, repo: &Path) -> Value {
    store
        .begin(
            scope,
            Begin {
                repo: repo.to_owned(),
            },
            &SubmoduleConfig::default(),
        )
        .unwrap()
}

fn page(store: &mut Snapshots, scope: &Scope, repo: &Path, snapshot: &str, cursor: usize) -> Value {
    store
        .page(
            scope,
            PageRequest {
                repo: repo.to_owned(),
                snapshot: snapshot.to_owned(),
                cursor,
            },
        )
        .unwrap()
}

fn item<'a>(changes: &'a [Value], path: &str, group: &str) -> &'a Value {
    changes
        .iter()
        .find(|item| item["path"] == path && item["group"] == group)
        .unwrap_or_else(|| panic!("missing {group} change {path}: {changes:?}"))
}

#[test]
fn reports_independent_index_worktree_rename_and_recursive_untracked_files() {
    let (temporary, path, repo) = setup();
    fs::write(path.join("mixed.txt"), "staged\n").unwrap();
    fs::rename(path.join("old.txt"), path.join("new.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("mixed.txt")).unwrap();
    index.remove_path(Path::new("old.txt")).unwrap();
    index.add_path(Path::new("new.txt")).unwrap();
    index.write().unwrap();
    fs::write(path.join("mixed.txt"), "unstaged\n").unwrap();
    fs::remove_file(path.join("delete.txt")).unwrap();
    fs::create_dir_all(path.join("nested/deeper")).unwrap();
    fs::write(path.join("nested/deeper/new.txt"), "new\n").unwrap();
    fs::write(path.join("ignored.txt"), "ignored\n").unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(
        item(records, "mixed.txt", "index")["kind"],
        "index_modified"
    );
    assert_eq!(
        item(records, "mixed.txt", "working")["kind"],
        "working_modified"
    );
    let renamed = item(records, "new.txt", "index");
    assert_eq!(renamed["kind"], "index_renamed");
    assert_eq!(renamed["original_path"], "old.txt");
    assert_eq!(
        item(records, "delete.txt", "working")["kind"],
        "working_deleted"
    );
    assert_eq!(
        item(records, "nested/deeper/new.txt", "untracked")["kind"],
        "untracked"
    );
    assert!(!records.iter().any(|record| record["path"] == "ignored.txt"));
}

#[test]
fn untracked_embedded_repository_is_a_directory_record_without_hiding_files() {
    let (temporary, path, _repo) = setup();
    let nested = path.join("nested-repo");
    fs::create_dir(&nested).unwrap();
    Repository::init(&nested).unwrap();
    fs::write(nested.join("nested.txt"), "nested\n").unwrap();
    fs::write(path.join("visible.txt"), "visible\n").unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(
        item(records, "nested-repo", "untracked")["kind"],
        "untracked_directory"
    );
    assert_eq!(
        item(records, "visible.txt", "untracked")["kind"],
        "untracked"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["path"] == "nested-repo/nested.txt")
    );
}

#[test]
fn unstaged_move_keeps_deletion_and_untracked_file_as_separate_actions() {
    let (temporary, path, _repo) = setup();
    fs::rename(path.join("old.txt"), path.join("moved.txt")).unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(
        item(records, "old.txt", "working")["kind"],
        "working_deleted"
    );
    assert_eq!(item(records, "moved.txt", "untracked")["kind"], "untracked");
    assert!(
        !records
            .iter()
            .any(|record| record["kind"] == "working_renamed")
    );
}

#[test]
fn conflict_records_preserve_the_available_index_stages() {
    let (temporary, path, repo) = setup();
    let mut index = repo.index().unwrap();
    let template = index.get_path(Path::new("mixed.txt"), 0).unwrap();
    for (name, stages) in [
        ("deleted.txt", &[1][..]),
        ("ours.txt", &[2][..]),
        ("theirs.txt", &[3][..]),
        ("deleted-by-ours.txt", &[1, 3][..]),
        ("deleted-by-theirs.txt", &[1, 2][..]),
        ("both.txt", &[2, 3][..]),
        ("modified.txt", &[1, 2, 3][..]),
        ("gitlink.txt", &[2][..]),
    ] {
        for stage in stages {
            let mut entry = git2::IndexEntry {
                ctime: template.ctime,
                mtime: template.mtime,
                dev: template.dev,
                ino: template.ino,
                mode: template.mode,
                uid: template.uid,
                gid: template.gid,
                file_size: template.file_size,
                id: template.id,
                flags: (template.flags & !(3 << 12)) | (stage << 12),
                flags_extended: template.flags_extended,
                path: name.as_bytes().to_vec(),
            };
            entry.id = repo.blob(format!("stage {stage}").as_bytes()).unwrap();
            if name == "gitlink.txt" {
                entry.mode = 0o160000;
                entry.id = repo.head().unwrap().target().unwrap();
            }
            index.add(&entry).unwrap();
        }
    }
    index.write().unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    for (name, expected) in [
        ("deleted.txt", [true, false, false]),
        ("ours.txt", [false, true, false]),
        ("theirs.txt", [false, false, true]),
        ("deleted-by-ours.txt", [true, false, true]),
        ("deleted-by-theirs.txt", [true, true, false]),
        ("both.txt", [false, true, true]),
        ("modified.txt", [true, true, true]),
    ] {
        let stages = &item(records, name, "merge")["conflict_stages"];
        for (key, present) in ["base", "ours", "theirs"].into_iter().zip(expected) {
            assert_eq!(
                stages[key].as_str(),
                present.then_some(name),
                "{name} {key}"
            );
        }
    }
    assert_eq!(item(records, "gitlink.txt", "merge")["is_submodule"], true);
}

#[test]
fn intent_to_add_is_a_worktree_addition_not_a_staged_file() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, path, _repo) = setup();
    fs::write(path.join("intent.txt"), "content\n").unwrap();
    let output = crate::git::process::git_command(&path)
        .args(["add", "-N", "--", "intent.txt"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(
        item(records, "intent.txt", "working")["kind"],
        "intent_to_add"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["path"] == "intent.txt" && record["group"] == "index")
    );
    fs::remove_file(path.join("intent.txt")).unwrap();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(
        item(records, "intent.txt", "working")["kind"],
        "working_deleted"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["path"] == "intent.txt" && record["group"] == "index")
    );
}

#[test]
fn submodule_pointer_changes_are_marked_as_submodules() {
    if !crate::git::git_test_available() {
        return;
    }
    let (temporary, path, _repo) = setup();
    let (_source_tmp, source, _source_repo) = setup();
    let output = crate::git::process::git_command(&path)
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "--"])
        .arg(crate::repo_id::boundary_path(&source).as_ref())
        .arg("child")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(item(records, "child", "index")["is_submodule"], true);
}

fn commit_index(repo: &Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
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

#[test]
fn removed_gitmodules_does_not_make_deleted_gitlink_an_ordinary_file() {
    let (temporary, path, repo) = setup();
    fs::write(
        path.join(".gitmodules"),
        "[submodule \"child\"]\n\tpath = child\n\turl = ../child\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".gitmodules")).unwrap();
    let template = index.get_path(Path::new("mixed.txt"), 0).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let child = git2::IndexEntry {
        ctime: template.ctime,
        mtime: template.mtime,
        dev: template.dev,
        ino: template.ino,
        mode: 0o160000,
        uid: template.uid,
        gid: template.gid,
        file_size: 0,
        id: head.id(),
        flags: template.flags,
        flags_extended: 0,
        path: b"child".to_vec(),
    };
    index.add(&child).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "add gitlink",
        &tree,
        &[&head],
    )
    .unwrap();
    drop(tree);
    fs::remove_file(path.join(".gitmodules")).unwrap();
    index.remove_path(Path::new(".gitmodules")).unwrap();
    index.remove_path(Path::new("child")).unwrap();
    index.write().unwrap();
    drop(index);
    drop(head);
    drop(repo);
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    let child = item(records, "child", "index");
    assert_eq!(child["kind"], "index_deleted");
    assert_eq!(child["is_submodule"], true);
}

#[test]
fn pages_are_revisitable_revision_is_stable_and_snapshots_are_scoped() {
    let (temporary, path, _repo) = setup();
    for index in 0..260 {
        fs::write(path.join(format!("untracked-{index:03}.txt")), "new").unwrap();
    }
    let other = temporary.path().join("other");
    fs::create_dir(&other).unwrap();
    Repository::init(&other).unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let first = begin(&mut store, &scope, &path);
    let token = first["snapshot"].as_str().unwrap();
    let next = first["next_cursor"].as_u64().unwrap() as usize;
    assert_eq!(first["changes"].as_array().unwrap().len(), 128);
    let second = page(&mut store, &scope, &path, token, next);
    assert_eq!(second["previous_cursor"], 0);
    assert_eq!(
        page(&mut store, &scope, &path, token, 0)["changes"],
        first["changes"]
    );
    let last = page(
        &mut store,
        &scope,
        &path,
        token,
        second["next_cursor"].as_u64().unwrap() as usize,
    );
    assert!(last["next_cursor"].is_null());
    let bad_cursor = store
        .page(
            &scope,
            PageRequest {
                repo: path.clone(),
                snapshot: token.into(),
                cursor: 1,
            },
        )
        .unwrap_err();
    assert_eq!(bad_cursor.code, "snapshot_stale");
    let wrong_repo = store
        .page(
            &scope,
            PageRequest {
                repo: other,
                snapshot: token.into(),
                cursor: 0,
            },
        )
        .unwrap_err();
    assert_eq!(wrong_repo.code, "snapshot_stale");

    let replacement = begin(&mut store, &scope, &path);
    assert_eq!(first["revision"], replacement["revision"]);
    assert_ne!(first["snapshot"], replacement["snapshot"]);
    let old = store
        .page(
            &scope,
            PageRequest {
                repo: path.clone(),
                snapshot: token.into(),
                cursor: 0,
            },
        )
        .unwrap_err();
    assert_eq!(old.code, "snapshot_stale");
    let replacement_token = replacement["snapshot"].as_str().unwrap();
    assert_eq!(
        page(&mut store, &scope, &path, replacement_token, next)["changes"],
        second["changes"]
    );
    store
        .release(
            &scope,
            Release {
                repo: path.clone(),
                snapshot: replacement_token.into(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .page(
                &scope,
                PageRequest {
                    repo: path,
                    snapshot: replacement_token.into(),
                    cursor: 0
                }
            )
            .unwrap_err()
            .code,
        "snapshot_stale"
    );
}

#[test]
fn many_repositories_remain_pageable_until_their_snapshots_are_replaced_or_closed() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let mut store = Snapshots::default();
    let mut opened: Vec<(PathBuf, String)> = Vec::new();

    for index in 0..130 {
        let repo = root.join(format!("repo-{index:03}"));
        fs::create_dir(&repo).unwrap();
        Repository::init(&repo).unwrap();
        fs::write(repo.join("change.txt"), format!("change {index}")).unwrap();
        let result = begin(&mut store, &scope, &repo);
        assert_eq!(result["changes"][0]["path"], "change.txt");
        opened.push((repo, result["snapshot"].as_str().unwrap().to_owned()));
    }
    for index in [0, 64, 129] {
        let (repo, snapshot) = &opened[index];
        assert_eq!(
            page(&mut store, &scope, repo, snapshot, 0)["changes"][0]["path"],
            "change.txt"
        );
    }
    let (first_repo, first_snapshot) = &opened[0];
    let replacement = begin(&mut store, &scope, first_repo);
    let replacement_token = replacement["snapshot"].as_str().unwrap().to_owned();
    assert_ne!(replacement_token, *first_snapshot);
    assert_eq!(
        store
            .page(
                &scope,
                PageRequest {
                    repo: first_repo.clone(),
                    snapshot: first_snapshot.clone(),
                    cursor: 0
                }
            )
            .unwrap_err()
            .code,
        "snapshot_stale"
    );
    store
        .release(
            &scope,
            Release {
                repo: first_repo.clone(),
                snapshot: replacement_token,
            },
        )
        .unwrap();
    let (last_repo, last_snapshot) = &opened[129];
    assert_eq!(
        page(&mut store, &scope, last_repo, last_snapshot, 0)["changes"][0]["path"],
        "change.txt"
    );
}

#[test]
fn find_returns_the_selected_change_across_pages_and_rejects_stale_scope() {
    let (_temporary, path, repo) = setup();
    fs::write(path.join("mixed.txt"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("mixed.txt")).unwrap();
    index.write().unwrap();
    fs::write(path.join("mixed.txt"), "working\n").unwrap();
    for index in 0..150 {
        fs::write(path.join(format!("untracked-{index:03}.txt")), "new\n").unwrap();
    }
    let scope = Scope::new(vec![path.clone()]).unwrap();
    let mut store = Snapshots::default();
    let first = begin(&mut store, &scope, &path);
    assert!(first["next_cursor"].is_number());
    let snapshot = first["snapshot"].as_str().unwrap().to_owned();
    let request = |file: &str, group: &str| FindRequest {
        repo: path.clone(),
        snapshot: snapshot.clone(),
        path: file.to_owned(),
        group: group.to_owned(),
    };
    assert_eq!(
        store.find(&scope, request("mixed.txt", "index")).unwrap()["change"]["kind"],
        "index_modified"
    );
    assert_eq!(
        store.find(&scope, request("mixed.txt", "working")).unwrap()["change"]["kind"],
        "working_modified"
    );
    assert_eq!(
        store
            .find(&scope, request("untracked-149.txt", "untracked"))
            .unwrap()["change"]["path"],
        "untracked-149.txt"
    );
    assert!(
        store
            .find(&scope, request("absent.txt", "working"))
            .unwrap()["change"]
            .is_null()
    );
    assert_eq!(
        store
            .find(&scope, request("../outside", "working"))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    assert_eq!(
        store
            .find(&Scope::new(vec![]).unwrap(), request("mixed.txt", "index"))
            .unwrap_err()
            .code,
        "outside_workspace"
    );
    store
        .release(
            &scope,
            Release {
                repo: path.clone(),
                snapshot: snapshot.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .find(&scope, request("mixed.txt", "index"))
            .unwrap_err()
            .code,
        "snapshot_stale"
    );
}

#[test]
fn replacing_a_repository_or_removing_it_from_scope_invalidates_pages() {
    let (temporary, path, repo) = setup();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let token = result["snapshot"].as_str().unwrap().to_owned();
    let empty_scope = Scope::new(vec![]).unwrap();
    assert_eq!(
        store
            .page(
                &empty_scope,
                PageRequest {
                    repo: path.clone(),
                    snapshot: token.clone(),
                    cursor: 0
                }
            )
            .unwrap_err()
            .code,
        "outside_workspace"
    );
    drop(repo);
    fs::rename(&path, temporary.path().join("old")).unwrap();
    fs::create_dir(&path).unwrap();
    Repository::init(&path).unwrap();
    assert_eq!(
        store
            .page(
                &scope,
                PageRequest {
                    repo: path,
                    snapshot: token,
                    cursor: 0
                }
            )
            .unwrap_err()
            .code,
        "snapshot_stale"
    );
}

#[test]
fn reinitialization_invalidates_snapshot_tokens_even_for_the_same_workspace() {
    let (temporary, path, _repo) = setup();
    let mut scope = None;
    let mut snapshots = Snapshots::default();
    let mut call = |method: &str, params: Value| {
        crate::bridge::process(
            &mut scope,
            &mut snapshots,
            serde_json::from_value(json!({"id": 1, "method": method, "params": params})).unwrap(),
            &crate::bridge::BridgeOptions {
                submodules: &SubmoduleConfig::default(),
                graph: &GraphConfig::default(),
                worktree: &WorktreeConfig::default(),
                review_base: None,
                theme: &crate::config::Config::default(),
                monitor_stdio: false,
            },
        )
    };
    let initialize = json!({"version": 1, "workspace_roots": [temporary.path()]});
    call("initialize", initialize.clone()).unwrap();
    let response = call("changes_begin", json!({"repo": path})).unwrap();
    let token = response["snapshot"].as_str().unwrap().to_owned();
    call("initialize", initialize).unwrap();
    let error = call(
        "changes_next",
        json!({"repo": path, "snapshot": token, "cursor": 0}),
    )
    .unwrap_err();
    assert_eq!(error.code, "snapshot_stale");
}

#[test]
fn page_boundaries_respect_encoded_size_and_reject_oversized_records() {
    let sizes = [100_000, 100_000, 100_000, 10];
    assert_eq!(super::page_boundaries(&sizes).unwrap(), [0, 2]);
    assert_eq!(
        super::page_boundaries(&[super::MAX_PAGE_BYTES])
            .unwrap_err()
            .code,
        "snapshot_too_large"
    );
    let mut total = 0;
    assert_eq!(
        super::account_record(&mut total, super::MAX_PAGE_BYTES)
            .unwrap_err()
            .code,
        "snapshot_too_large"
    );
    let mut total = super::MAX_CACHE_BYTES - 129;
    assert_eq!(
        super::account_record(&mut total, 2).unwrap_err().code,
        "snapshot_too_large"
    );
}

#[test]
fn cache_accounting_charges_empty_snapshots_paths_and_retained_table_capacity() {
    let (_temporary, path, _repo) = setup();
    let identity = super::RepositoryIdentity::read(&path).unwrap();
    let mut snapshot = super::Snapshot {
        id: "changes-1".to_owned(),
        identity,
        revision: "a".repeat(64),
        records: Vec::new(),
        page_starts: vec![0],
        bytes: 0,
        touched: std::time::Instant::now(),
    };
    let empty = super::snapshot_size(&path, &snapshot);
    assert!(empty > 0);
    snapshot.identity.common.path = PathBuf::from("x".repeat(4096));
    snapshot.page_starts = Vec::with_capacity(512);
    assert!(super::snapshot_size(&path, &snapshot) >= empty + 4096);
    assert_eq!(super::table_size(usize::MAX), usize::MAX);
    let mut store = Snapshots::default();
    store.entries.reserve(128);
    let retained_table = super::table_size(store.entries.capacity());
    assert!(retained_table > 0);
    assert!(!store.fits_capacity(&path, super::MAX_CACHE_BYTES, store.entries.capacity()));
    store.clear();
    assert_eq!(super::table_size(store.entries.capacity()), 0);
}

#[test]
fn replacement_charges_the_retained_repository_key_allocation() {
    let (temporary, path, _repo) = setup();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    begin(&mut store, &scope, &path);
    let (key, mut snapshot) = store.entries.remove_entry(&path).unwrap();
    let mut larger_key = PathBuf::with_capacity(key.capacity() + 16_384);
    larger_key.push(&key);
    snapshot.bytes += larger_key.capacity() - key.capacity();
    let previous_charge = snapshot.bytes;
    store.bytes = previous_charge;
    store.entries.insert(larger_key, snapshot);

    begin(&mut store, &scope, &path);
    let (retained_key, replacement) = store.entries.get_key_value(&path).unwrap();
    assert!(retained_key.capacity() >= key.capacity() + 16_384);
    assert!(replacement.bytes >= previous_charge);
}

#[test]
fn full_cache_preserves_the_previous_snapshot_until_capacity_is_released() {
    let (temporary, first, _repo) = setup();
    let second = temporary.path().join("second");
    fs::create_dir(&second).unwrap();
    Repository::init(&second).unwrap();
    let second = second.canonicalize().unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let first_snapshot = begin(&mut store, &scope, &first)["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_snapshot = begin(&mut store, &scope, &second)["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    // Model a second repository whose retained records fill the remaining
    // cache; the repository and replacement requests still use real Git data.
    let first_bytes = store.entries.get(&first).unwrap().bytes;
    let available = super::MAX_CACHE_BYTES - super::table_size(store.entries.capacity());
    store.entries.get_mut(&second).unwrap().bytes = available - first_bytes;
    store.bytes = available;
    fs::write(first.join("new.txt"), "new").unwrap();
    let error = store
        .begin(
            &scope,
            Begin {
                repo: first.clone(),
            },
            &SubmoduleConfig::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, "snapshot_capacity");
    assert!(
        page(&mut store, &scope, &first, &first_snapshot, 0)["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    store
        .release(
            &scope,
            Release {
                repo: second,
                snapshot: second_snapshot,
            },
        )
        .unwrap();
    let replacement = begin(&mut store, &scope, &first);
    assert_eq!(replacement["changes"][0]["path"], "new.txt");
}

#[test]
fn expired_snapshot_reports_stale() {
    let (temporary, path, _repo) = setup();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let token = result["snapshot"].as_str().unwrap().to_owned();
    store.entries.get_mut(&path).unwrap().touched = std::time::Instant::now() - super::EXPIRY;
    assert_eq!(
        store
            .page(
                &scope,
                PageRequest {
                    repo: path,
                    snapshot: token,
                    cursor: 0
                }
            )
            .unwrap_err()
            .code,
        "snapshot_stale"
    );
}

#[test]
fn change_record_limit_is_an_explicit_error() {
    let (_temporary, path, repo) = setup();
    fs::write(path.join("one.txt"), "new").unwrap();
    fs::write(path.join("two.txt"), "new").unwrap();
    let result = crate::git::status::collect_changes(&repo, &SubmoduleConfig::default(), 1);
    assert!(matches!(
        result,
        Err(crate::git::status::ChangeError::Limit)
    ));
}
