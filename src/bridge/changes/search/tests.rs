use std::fs;
use std::path::{Path, PathBuf};

use git2::Repository;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::bridge::changes::{Begin, PageRequest, Snapshots};
use crate::bridge::scope::Scope;
use crate::config::SubmoduleConfig;

use super::SearchRequest;

fn fixture() -> (TempDir, PathBuf, Scope, Snapshots, String) {
    let temporary = TempDir::new().unwrap();
    let repo = temporary.path().join("repo");
    fs::create_dir(&repo).unwrap();
    Repository::init(&repo).unwrap();
    for number in 0..260 {
        let suffix = if [0, 128, 259].contains(&number) {
            "-ÉCHO"
        } else {
            ""
        };
        fs::write(repo.join(format!("file-{number:03}{suffix}.txt")), "x").unwrap();
    }
    let repo = repo.canonicalize().unwrap();
    let scope = Scope::new(vec![temporary.path().to_owned()]).unwrap();
    let mut store = Snapshots::default();
    let first = store
        .begin(
            &scope,
            Begin { repo: repo.clone() },
            &SubmoduleConfig::default(),
        )
        .unwrap();
    let snapshot = first["snapshot"].as_str().unwrap().to_owned();
    (temporary, repo, scope, store, snapshot)
}

fn request(
    repo: &Path,
    snapshot: &str,
    query: &str,
    direction: &str,
    position: Value,
) -> SearchRequest {
    serde_json::from_value(json!({
        "repo": repo, "snapshot": snapshot, "query": query,
        "direction": direction, "position": position,
    }))
    .unwrap()
}

#[test]
fn finds_paths_across_snapshot_pages_and_wraps_case_insensitively() {
    let (_temporary, repo, scope, mut store, snapshot) = fixture();
    let first = store
        .search(
            &scope,
            request(&repo, &snapshot, "éChO", "first", Value::Null),
        )
        .unwrap();
    assert_eq!(first["count"], 3);
    assert_eq!(first["position"], 0);
    assert_eq!(first["path"], "file-000-ÉCHO.txt");
    assert_eq!(first["group"], "untracked");
    assert_eq!(first["cursor"], 0);
    assert_eq!(first["page"], 0);

    let second = store
        .search(&scope, request(&repo, &snapshot, "écho", "next", json!(0)))
        .unwrap();
    assert_eq!(second["position"], 1);
    assert_eq!(second["path"], "file-128-ÉCHO.txt");
    assert_eq!(second["cursor"], 128);
    assert_eq!(second["page"], 1);
    let page = store
        .page(
            &scope,
            PageRequest {
                repo: repo.clone(),
                snapshot: snapshot.clone(),
                cursor: 128,
            },
        )
        .unwrap();
    assert_eq!(page["revision"], second["revision"]);
    assert!(
        page["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["path"] == second["path"] && item["group"] == second["group"] })
    );
    let last = store
        .search(
            &scope,
            request(&repo, &snapshot, "écho", "previous", json!(0)),
        )
        .unwrap();
    assert_eq!(last["position"], 2);
    assert_eq!(last["path"], "file-259-ÉCHO.txt");
    assert_eq!(last["cursor"], 256);
    assert_eq!(last["page"], 2);
    let wrapped = store
        .search(&scope, request(&repo, &snapshot, "écho", "next", json!(2)))
        .unwrap();
    assert_eq!(wrapped["path"], first["path"]);

    let none = store
        .search(
            &scope,
            request(&repo, &snapshot, "missing", "first", Value::Null),
        )
        .unwrap();
    assert_eq!(none["count"], 0);
    for name in ["position", "cursor", "page", "path", "group"] {
        assert!(none[name].is_null(), "{name}");
    }
}

#[test]
fn rejects_invalid_queries_positions_and_stale_or_other_repositories() {
    let (temporary, repo, scope, mut store, snapshot) = fixture();
    for query in ["".to_owned(), "a".repeat(513), "bad\0query".to_owned()] {
        let error = store
            .search(
                &scope,
                request(&repo, &snapshot, &query, "first", Value::Null),
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }
    let error = store
        .search(&scope, request(&repo, &snapshot, "écho", "next", json!(3)))
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    let other = temporary.path().join("other");
    fs::create_dir(&other).unwrap();
    Repository::init(&other).unwrap();
    let error = store
        .search(
            &scope,
            request(&other, &snapshot, "écho", "first", Value::Null),
        )
        .unwrap_err();
    assert_eq!(error.code, "snapshot_stale");
    let replacement = store
        .begin(
            &scope,
            Begin { repo: repo.clone() },
            &SubmoduleConfig::default(),
        )
        .unwrap();
    let error = store
        .search(
            &scope,
            request(&repo, &snapshot, "écho", "first", Value::Null),
        )
        .unwrap_err();
    assert_eq!(error.code, "snapshot_stale");
    let replacement_snapshot = replacement["snapshot"].as_str().unwrap();
    fs::rename(&repo, temporary.path().join("original")).unwrap();
    fs::create_dir(&repo).unwrap();
    Repository::init(&repo).unwrap();
    let error = store
        .search(
            &scope,
            request(&repo, replacement_snapshot, "écho", "first", Value::Null),
        )
        .unwrap_err();
    assert_eq!(error.code, "snapshot_stale");
}
