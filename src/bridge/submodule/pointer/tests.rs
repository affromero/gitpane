mod diagnostic_tests;

use super::*;
use git2::{Repository, Signature};

fn setup() -> (tempfile::TempDir, PathBuf, Repository, Scope) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("repo");
    std::fs::create_dir(&path).unwrap();
    let repo = Repository::init(&path).unwrap();
    std::fs::write(path.join("seed"), "seed").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("seed")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    let scope = Scope::new(vec![path.clone()]).unwrap();
    (temp, path, repo, scope)
}

fn put(repo: &Repository, path: &str, mode: u32, oid: git2::Oid, stage: u16) {
    let mut index = repo.index().unwrap();
    let template = index.get_path(Path::new("seed"), 0).unwrap();
    let entry = git2::IndexEntry {
        ctime: template.ctime,
        mtime: template.mtime,
        dev: template.dev,
        ino: template.ino,
        mode,
        uid: template.uid,
        gid: template.gid,
        file_size: 0,
        id: oid,
        flags: (template.flags & !(3 << 12)) | (stage << 12),
        flags_extended: template.flags_extended,
        path: path.as_bytes().to_vec(),
    };
    index.add(&entry).unwrap();
    index.write().unwrap();
}

fn commit(repo: &Repository) {
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "update", &tree, &[&parent])
        .unwrap();
}

fn request(repo: &Repository, path: &Path, name: &str, group: &str) -> CompareRequest {
    let record = changes::current_record(repo, &SubmoduleConfig::default(), name, group)
        .unwrap()
        .unwrap();
    assert_eq!(record["is_submodule"], true, "{record:?}");
    CompareRequest {
        repo: path.to_owned(),
        record,
        expected_repo_identity: RepositoryIdentity::read(path)
            .unwrap()
            .fingerprint()
            .unwrap(),
    }
}

fn read(
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

#[test]
fn staged_gitlinks_use_head_and_index_and_reject_same_status_oid_changes() {
    let (_temp, path, repo, scope) = setup();
    let first = repo.head().unwrap().target().unwrap();
    put(&repo, "child", 0o160000, first, 0);
    let added = request(&repo, &path, "child", "index");
    let added_snapshot = compare(&scope, added, &SubmoduleConfig::default()).unwrap();
    assert!(added_snapshot["sides"]["base"].is_null());
    assert_eq!(added_snapshot["sides"]["index"]["oid"], first.to_string());
    commit(&repo);
    let second = repo.head().unwrap().target().unwrap();
    put(&repo, "child", 0o160000, second, 0);
    let selected = request(&repo, &path, "child", "index");
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    assert_eq!(snapshot["sides"]["base"]["oid"], first.to_string());
    assert_eq!(snapshot["sides"]["index"]["oid"], second.to_string());
    assert_eq!(
        read(&scope, &selected, &snapshot, "index").unwrap()["content"],
        format!("Subproject commit {second}\n")
    );
    let third = git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    put(&repo, "child", 0o160000, third, 0);
    assert_eq!(
        changes::current_record(&repo, &SubmoduleConfig::default(), "child", "index")
            .unwrap()
            .unwrap(),
        selected.record
    );
    assert_eq!(
        read(&scope, &selected, &snapshot, "index")
            .unwrap_err()
            .code,
        "stale_target"
    );
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("child")).unwrap();
    index.write().unwrap();
    let deleted = request(&repo, &path, "child", "index");
    let snapshot = compare(&scope, deleted, &SubmoduleConfig::default()).unwrap();
    assert_eq!(snapshot["sides"]["base"]["kind"], "gitlink");
    assert!(snapshot["sides"]["index"].is_null());
}

#[test]
fn conflicted_stages_include_blob_and_gitlink_and_guard_changed_blob() {
    let (_temp, path, repo, scope) = setup();
    let oid = repo.head().unwrap().target().unwrap();
    let blob = repo.blob(b"first version\n").unwrap();
    put(&repo, "child", 0o160000, oid, 1);
    put(&repo, "child", 0o100644, blob, 2);
    put(&repo, "child", 0o160000, oid, 3);
    let selected = request(&repo, &path, "child", "merge");
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    assert_eq!(snapshot["sides"]["base"]["kind"], "gitlink");
    assert_eq!(snapshot["sides"]["ours"]["kind"], "blob");
    assert_eq!(
        read(&scope, &selected, &snapshot, "ours").unwrap()["content"],
        "first version\n"
    );
    let replacement = repo.blob(b"second version\n").unwrap();
    put(&repo, "child", 0o100644, replacement, 2);
    assert_eq!(
        read(&scope, &selected, &snapshot, "ours").unwrap_err().code,
        "stale_target"
    );
    for bytes in [vec![b'x'; MAX_CONTENT + 1], b"binary\0data".to_vec()] {
        let blob = repo.blob(&bytes).unwrap();
        put(&repo, "child", 0o100644, blob, 2);
        let selected = request(&repo, &path, "child", "merge");
        let snapshot = compare(
            &scope,
            CompareRequest {
                repo: selected.repo.clone(),
                record: selected.record.clone(),
                expected_repo_identity: selected.expected_repo_identity.clone(),
            },
            &SubmoduleConfig::default(),
        )
        .unwrap();
        assert_eq!(
            read(&scope, &selected, &snapshot, "ours").unwrap_err().code,
            "invalid_request"
        );
    }
}

#[test]
fn staged_gitlink_typechange_exposes_both_real_parent_sides() {
    let (_temp, path, repo, scope) = setup();
    let oid = repo.head().unwrap().target().unwrap();
    put(&repo, "child", 0o160000, oid, 0);
    commit(&repo);
    let blob = repo.blob(b"replacement\n").unwrap();
    put(&repo, "child", 0o100644, blob, 0);
    let selected = request(&repo, &path, "child", "index");
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    assert_eq!(snapshot["sides"]["base"]["kind"], "gitlink");
    assert_eq!(snapshot["sides"]["index"]["kind"], "blob");
    assert_eq!(
        read(&scope, &selected, &snapshot, "index").unwrap()["content"],
        "replacement\n"
    );
}

#[test]
fn unborn_addition_and_missing_conflict_stage_remain_empty() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unborn");
    std::fs::create_dir(&path).unwrap();
    let repo = Repository::init(&path).unwrap();
    std::fs::write(path.join("seed"), "seed").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("seed")).unwrap();
    index.write().unwrap();
    let arbitrary = git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    put(&repo, "child", 0o160000, arbitrary, 0);
    let scope = Scope::new(vec![path.clone()]).unwrap();
    let selected = request(&repo, &path, "child", "index");
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    assert!(snapshot["head_oid"].is_null());
    assert!(snapshot["sides"]["base"].is_null());
    assert_eq!(
        read(&scope, &selected, &snapshot, "base").unwrap()["content"],
        ""
    );
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("child")).unwrap();
    index.write().unwrap();
    put(&repo, "child", 0o160000, arbitrary, 2);
    let selected = request(&repo, &path, "child", "merge");
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    assert!(snapshot["sides"]["base"].is_null());
    assert!(snapshot["sides"]["theirs"].is_null());
    assert_eq!(snapshot["sides"]["ours"]["oid"], arbitrary.to_string());
}

#[test]
fn content_rejects_untrusted_snapshots_and_outside_scope() {
    let (_temp, path, repo, scope) = setup();
    let oid = repo.head().unwrap().target().unwrap();
    put(&repo, "child", 0o160000, oid, 0);
    let selected = request(&repo, &path, "child", "index");
    let mut snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    snapshot["sides"]["index"]["oid"] = json!("1".repeat(40));
    assert_eq!(
        read(&scope, &selected, &snapshot, "index")
            .unwrap_err()
            .code,
        "stale_target"
    );
    let other = tempfile::tempdir().unwrap();
    let outside = Scope::new(vec![other.path().to_owned()]).unwrap();
    assert_eq!(
        compare(&outside, selected, &SubmoduleConfig::default())
            .unwrap_err()
            .code,
        "outside_workspace"
    );
}

fn expected(snapshot: &Value) -> ExpectedSnapshot {
    ExpectedSnapshot {
        record: snapshot["record"].clone(),
        head_oid: snapshot["head_oid"].as_str().map(str::to_owned),
        sides: snapshot["sides"].clone(),
    }
}

#[test]
fn batch_validates_many_snapshots_and_identifies_one_changed_pointer() {
    let (_temp, path, repo, scope) = setup();
    let oid = repo.head().unwrap().target().unwrap();
    put(&repo, "one", 0o160000, oid, 0);
    put(&repo, "two", 0o160000, oid, 0);
    let one = request(&repo, &path, "one", "index");
    let two = request(&repo, &path, "two", "index");
    let one_snapshot = compare(
        &scope,
        CompareRequest {
            repo: one.repo.clone(),
            record: one.record.clone(),
            expected_repo_identity: one.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    let two_snapshot = compare(
        &scope,
        CompareRequest {
            repo: two.repo.clone(),
            record: two.record.clone(),
            expected_repo_identity: two.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    let make_request = || BatchRequest {
        repo: path.clone(),
        expected_repo_identity: one.expected_repo_identity.clone(),
        snapshots: (0..128)
            .map(|i| {
                expected(if i == 127 {
                    &two_snapshot
                } else {
                    &one_snapshot
                })
            })
            .collect(),
    };
    let response = validate_batch(&scope, make_request(), &SubmoduleConfig::default()).unwrap();
    assert!(
        response["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["valid"] == true)
    );
    let replacement = git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    put(&repo, "two", 0o160000, replacement, 0);
    assert_eq!(
        changes::current_record(&repo, &SubmoduleConfig::default(), "two", "index")
            .unwrap()
            .unwrap(),
        two.record
    );
    let response = validate_batch(&scope, make_request(), &SubmoduleConfig::default()).unwrap();
    let results = response["results"].as_array().unwrap();
    assert!(results[..127].iter().all(|result| result["valid"] == true));
    assert_eq!(results[127]["valid"], false);
    assert_eq!(results[127]["code"], "stale_target");

    let mut malformed = make_request();
    malformed.snapshots[0].record =
        json!({"path":"../escape", "group":"index", "is_submodule":true});
    let response = validate_batch(&scope, malformed, &SubmoduleConfig::default()).unwrap();
    assert_eq!(response["results"][0]["code"], "invalid_request");
    assert_eq!(response["results"][1]["valid"], true);
    let mut too_many = make_request();
    too_many.snapshots.push(expected(&one_snapshot));
    assert_eq!(
        validate_batch(&scope, too_many, &SubmoduleConfig::default())
            .unwrap_err()
            .code,
        "invalid_request"
    );
    let outside = tempfile::tempdir().unwrap();
    let other_scope = Scope::new(vec![outside.path().to_owned()]).unwrap();
    assert_eq!(
        validate_batch(&other_scope, make_request(), &SubmoduleConfig::default())
            .unwrap_err()
            .code,
        "outside_workspace"
    );
}
