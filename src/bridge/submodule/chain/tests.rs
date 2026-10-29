use std::fs;
use std::path::Path;

use git2::Repository;
use serde_json::json;

use super::*;
use crate::bridge::submodule::tests::{commit_file, fixture};

fn make_link(scope: &Scope, repo: &Path, path: &str) -> Link {
    let inspect = InspectRequest {
        repo: repo.to_path_buf(),
        path: path.into(),
        summary_only: true,
        group: Some(Group::Working),
        original_path: None,
    };
    let snapshot = read_link(scope, &inspect).unwrap();
    let parent = Repository::open(repo).unwrap();
    let record = changes::current_record(&parent, &SubmoduleConfig::default(), path, "working")
        .unwrap()
        .expect("working submodule change");
    Link {
        repo: repo.to_path_buf(),
        path: path.into(),
        group: Group::Working,
        original_path: None,
        record,
        child_repo: snapshot.child,
        base_oid: snapshot.base.map(|oid| oid.to_string()),
        head_oid: snapshot.head.map(|oid| oid.to_string()),
        child_head_oid: snapshot.child_head.to_string(),
        parent_identity: snapshot.parent_identity.fingerprint().unwrap(),
        child_identity: snapshot.child_identity.fingerprint().unwrap(),
    }
}

fn nested() -> (tempfile::TempDir, Scope, Vec<Link>) {
    let (temporary, parent, child, scope) = fixture();
    let child_repo = Repository::open(&child).unwrap();
    let source = temporary.path().join("source");
    let mut module = child_repo
        .submodule(source.to_str().unwrap(), Path::new("nested"), true)
        .unwrap();
    module.clone(None).unwrap();
    module.add_finalize().unwrap();
    drop(module);
    commit_file(&child_repo, "base", "base");
    fs::write(child.join("nested/base"), "dirty nested file").unwrap();
    scope.validate_submodules(&parent).unwrap();
    let links = vec![
        make_link(&scope, &parent, "child"),
        make_link(&scope, &child, "nested"),
    ];
    (temporary, scope, links)
}

fn check(scope: &Scope, links: Vec<Link>) -> Result<Value, BridgeError> {
    validate(scope, Request { chain: links }, &SubmoduleConfig::default())
}

#[test]
fn validates_nested_chain_and_detects_each_changed_record_and_pointer() {
    let (_temp, scope, links) = nested();
    assert_eq!(
        check(&scope, links.clone()).unwrap(),
        json!({"valid": true})
    );
    let mut aliases = links.clone();
    aliases[0].child_repo = aliases[0].child_repo.join("..").join("child");
    aliases[1].repo = aliases[1].repo.join("..").join("child");
    assert_eq!(check(&scope, aliases).unwrap(), json!({"valid": true}));
    for index in 0..links.len() {
        let mut changed = links.clone();
        changed[index].record["kind"] = json!("working_deleted");
        assert_eq!(check(&scope, changed).unwrap(), json!({"valid": false}));
        let mut changed = links.clone();
        changed[index].child_head_oid = "0000000000000000000000000000000000000000".to_string();
        assert_eq!(check(&scope, changed).unwrap(), json!({"valid": false}));
        let mut changed = links.clone();
        changed[index].child_identity = "0".repeat(64);
        assert_eq!(check(&scope, changed).unwrap(), json!({"valid": false}));
        let mut changed = links.clone();
        changed[index].parent_identity = "0".repeat(64);
        assert_eq!(check(&scope, changed).unwrap(), json!({"valid": false}));
    }
    let leaf = Repository::open(&links[1].child_repo).unwrap();
    commit_file(&leaf, "later", "new commit");
    assert_eq!(check(&scope, links).unwrap(), json!({"valid": false}));

    let (_temp, scope, links) = nested();
    let child = Repository::open(&links[0].child_repo).unwrap();
    let leaf = Repository::open(&links[1].child_repo).unwrap();
    commit_file(&child, "new-child-file", "new child commit");
    commit_file(&leaf, "new-leaf-file", "new leaf commit");
    assert_eq!(check(&scope, links).unwrap(), json!({"valid": false}));
}

#[test]
fn rejects_invalid_chains_and_unsupported_changes() {
    let (_temp, scope, links) = nested();
    assert_eq!(check(&scope, vec![]).unwrap(), json!({"valid": true}));
    assert_eq!(
        check(&scope, vec![links[0].clone(); 33]).unwrap_err().code,
        "chain_too_large"
    );
    assert_eq!(
        check(&scope, vec![links[0].clone(), links[0].clone()])
            .unwrap_err()
            .code,
        "invalid_request"
    );
    let mut invalid = links[0].clone();
    invalid.record["path"] = json!("other");
    assert_eq!(
        check(&scope, vec![invalid]).unwrap_err().code,
        "invalid_request"
    );
    let mut conflict = links[0].clone();
    conflict.group = Group::Merge;
    assert!(
        check(&scope, vec![conflict])
            .unwrap_err()
            .message
            .contains("Conflicted")
    );
    let mut rename = links[0].clone();
    rename.group = Group::Index;
    rename.original_path = Some("old".into());
    rename.record["group"] = json!("index");
    rename.record["kind"] = json!("index_renamed");
    rename.record["original_path"] = json!("old");
    assert!(
        check(&scope, vec![rename])
            .unwrap_err()
            .message
            .contains("renames")
    );
    let mut bad_oid = links[0].clone();
    bad_oid.child_head_oid = "abcdef".to_string();
    assert_eq!(
        check(&scope, vec![bad_oid]).unwrap_err().code,
        "invalid_request"
    );
    let mut bad_identity = links[0].clone();
    bad_identity.child_identity = "invalid".to_owned();
    assert_eq!(
        check(&scope, vec![bad_identity]).unwrap_err().code,
        "invalid_request"
    );
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for item in fs::read_dir(source).unwrap() {
        let item = item.unwrap();
        let source = item.path();
        let target = destination.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_directory(&source, &target);
        } else {
            fs::copy(source, target).unwrap();
        }
    }
}

#[test]
fn rejects_repointed_gitdir_even_when_the_visible_state_is_identical() {
    let (_temp, scope, links) = nested();
    let repo = Repository::open(&links[0].child_repo).unwrap();
    let original = repo.path().canonicalize().unwrap();
    drop(repo);
    let replacement = original.parent().unwrap().join("replacement-child");
    copy_directory(&original, &replacement);
    fs::write(
        links[0].child_repo.join(".git"),
        format!("gitdir: {}\n", replacement.display()),
    )
    .unwrap();
    let replacement_repo = Repository::open(&links[0].child_repo).unwrap();
    assert_eq!(
        replacement_repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string(),
        links[0].child_head_oid
    );
    let record = changes::current_record(
        &Repository::open(&links[0].repo).unwrap(),
        &SubmoduleConfig::default(),
        "child",
        "working",
    )
    .unwrap();
    assert_eq!(record.as_ref(), Some(&links[0].record));
    assert_eq!(check(&scope, links).unwrap(), json!({"valid": false}));
}

#[test]
fn rejects_current_gitlink_file_type_change() {
    let (_temp, scope, links) = nested();
    let repo = Repository::open(&links[0].repo).unwrap();
    let mut index = repo.index().unwrap();
    let mut entry = index.get_path(Path::new("child"), 0).unwrap();
    entry.mode = 0o100644;
    entry.id = repo.blob(b"replacement").unwrap();
    index.add(&entry).unwrap();
    index.write().unwrap();
    let mut changed = links[0].clone();
    changed.group = Group::Index;
    changed.record = changes::current_record(&repo, &SubmoduleConfig::default(), "child", "index")
        .unwrap()
        .expect("staged type change");
    let error = check(&scope, vec![changed]).unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert!(error.message.contains("type changes cannot be inspected"));
}

#[test]
fn bridge_validation_preserves_open_change_snapshots() {
    use crate::bridge::{BridgeOptions, Request as BridgeRequest, changes::Snapshots, process};
    use crate::config::Config;

    let (temp, _fixture_scope, links) = nested();
    let config = Config::default();
    let options = BridgeOptions {
        submodules: &config.submodules,
        graph: &config.graph,
        worktree: &config.worktree,
        review_base: None,
        theme: &config,
    };
    let mut scope = None;
    let mut snapshots = Snapshots::default();
    let mut call = |method: &str, params: Value| {
        let request: BridgeRequest =
            serde_json::from_value(json!({"id": 1, "method": method, "params": params})).unwrap();
        process(&mut scope, &mut snapshots, request, &options).unwrap()
    };
    call(
        "initialize",
        json!({"version": 1, "workspace_roots": [temp.path().join("workspace")]}),
    );
    let page = call("changes_begin", json!({"repo": links[0].repo}));
    let token = page["snapshot"].as_str().unwrap();
    let chain: Vec<_> = links.iter().map(|link| json!({
        "repo": link.repo, "path": link.path, "group": "working", "original_path": null,
        "record": link.record, "child_repo": link.child_repo,
            "base_oid": link.base_oid, "head_oid": link.head_oid, "child_head_oid": link.child_head_oid,
            "parent_identity": link.parent_identity, "child_identity": link.child_identity
    })).collect();
    assert_eq!(
        call("submodule_chain_validate", json!({"chain": chain})),
        json!({"valid": true})
    );
    let result = call(
        "changes_find",
        json!({"repo": links[0].repo, "snapshot": token, "path": "child", "group": "working"}),
    );
    assert_eq!(result["change"], links[0].record);
}

#[cfg(unix)]
#[test]
fn rejects_a_nested_submodule_that_moves_outside_the_workspace() {
    let (temp, scope, links) = nested();
    let target = links[1].child_repo.clone();
    let saved = temp.path().join("saved-nested");
    fs::rename(&target, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &target).unwrap();
    assert_eq!(check(&scope, links).unwrap_err().code, "outside_workspace");
}
