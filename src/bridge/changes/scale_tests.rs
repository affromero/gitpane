use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use git2::Repository;
use serde_json::Value;

use super::{Begin, EXPIRY, MAX_CACHE_BYTES, PageRequest, Release, Snapshots, table_size};
use crate::bridge::scope::Scope;
use crate::config::SubmoduleConfig;

fn repo(root: &Path, index: usize) -> PathBuf {
    let path = root.join(format!("repo-{index:03}"));
    fs::create_dir(&path).unwrap();
    Repository::init(&path).unwrap();
    path.canonicalize().unwrap()
}

fn begin(store: &mut Snapshots, scope: &Scope, path: &Path) -> Value {
    store
        .begin(
            scope,
            Begin {
                repo: path.to_owned(),
            },
            &SubmoduleConfig::default(),
        )
        .unwrap()
}

#[test]
fn failed_new_repository_reservation_preserves_the_live_table() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let mut store = Snapshots::default();
    let mut opened = Vec::new();
    loop {
        let path = repo(&root, opened.len());
        let response = begin(&mut store, &scope, &path);
        opened.push((path, response["snapshot"].as_str().unwrap().to_owned()));
        if store.entries.len() == store.entries.capacity() {
            break;
        }
    }
    let extra = repo(&root, opened.len());
    let capacity = store.entries.capacity();
    let (last_path, _) = opened.last().unwrap();
    // Model retained records that consume the remaining budget without
    // constructing thousands of long Git index entries in this boundary test.
    let remaining = MAX_CACHE_BYTES - table_size(store.entries.capacity()) - store.bytes;
    store.entries.get_mut(last_path).unwrap().bytes += remaining;
    store.bytes += remaining;
    let error = store
        .begin(&scope, Begin { repo: extra }, &SubmoduleConfig::default())
        .unwrap_err();
    assert_eq!(error.code, "snapshot_capacity");
    assert_eq!(store.entries.capacity(), capacity);
    assert!(store.bytes + table_size(store.entries.capacity()) <= MAX_CACHE_BYTES);
    let (first, token) = &opened[0];
    let page = store
        .page(
            &scope,
            PageRequest {
                repo: first.clone(),
                snapshot: token.clone(),
                cursor: 0,
            },
        )
        .unwrap();
    assert!(page["changes"].as_array().unwrap().is_empty());
}

#[test]
fn closing_many_repositories_reclaims_table_capacity_without_losing_the_remaining_snapshot() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let mut store = Snapshots::default();
    let mut opened = Vec::new();
    for index in 0..130 {
        let path = repo(&root, index);
        let response = begin(&mut store, &scope, &path);
        opened.push((path, response["snapshot"].as_str().unwrap().to_owned()));
    }
    let capacity = store.entries.capacity();
    for (path, token) in &opened[..129] {
        store
            .release(
                &scope,
                Release {
                    repo: path.clone(),
                    snapshot: token.clone(),
                },
            )
            .unwrap();
    }
    assert!(store.entries.capacity() < capacity);
    assert!(store.bytes + table_size(store.entries.capacity()) <= MAX_CACHE_BYTES);
    let (last, token) = &opened[129];
    let page = store
        .page(
            &scope,
            PageRequest {
                repo: last.clone(),
                snapshot: token.clone(),
                cursor: 0,
            },
        )
        .unwrap();
    assert!(page["changes"].as_array().unwrap().is_empty());
}

#[test]
fn direct_access_expires_the_selected_snapshot_between_sweeps() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let mut store = Snapshots::default();
    let first = repo(&root, 0);
    let second = repo(&root, 1);
    let first_token = begin(&mut store, &scope, &first)["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_token = begin(&mut store, &scope, &second)["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    store.entries.get_mut(&first).unwrap().touched = Instant::now() - EXPIRY;
    store.last_expiry = Some(Instant::now());
    let error = store
        .page(
            &scope,
            PageRequest {
                repo: first,
                snapshot: first_token,
                cursor: 0,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "snapshot_stale");
    let remaining = store
        .page(
            &scope,
            PageRequest {
                repo: second,
                snapshot: second_token,
                cursor: 0,
            },
        )
        .unwrap();
    assert!(remaining["changes"].as_array().unwrap().is_empty());
}
