use std::fs;

use git2::Repository;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::bridge::{BridgeOptions, Request, changes::Snapshots, process, process_mutation};
use crate::config::Config;

fn options(config: &Config) -> BridgeOptions<'_> {
    BridgeOptions {
        submodules: &config.submodules,
        graph: &config.graph,
        worktree: &config.worktree,
        review_base: None,
        theme: config,
        monitor_stdio: false,
    }
}

fn request(method: &str, params: Value) -> Request {
    serde_json::from_value(json!({"id": 1, "method": method, "params": params})).unwrap()
}

#[test]
fn scoped_repository_identity_validation_detects_gitdir_replacement() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo = workspace.join("repo");
    let outside = temporary.path().join("outside");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&outside).unwrap();
    Repository::init(&repo).unwrap();
    Repository::init(&outside).unwrap();
    let repo = repo.canonicalize().unwrap();
    let token = crate::bridge::changes::RepositoryIdentity::read(&repo)
        .unwrap()
        .fingerprint()
        .unwrap();
    let mut scope = Some(super::Scope::new(vec![workspace]).unwrap());
    let mut snapshots = Snapshots::default();
    let config = Config::default();
    let call = |scope: &mut Option<super::Scope>,
                snapshots: &mut Snapshots,
                path: &std::path::Path,
                expected: &str| {
        process(
            scope,
            snapshots,
            request(
                "repo_identity_validate",
                json!({"repo":path,"expected_repo_identity":expected}),
            ),
            &options(&config),
        )
    };
    assert_eq!(
        call(&mut scope, &mut snapshots, &repo, &token).unwrap(),
        json!({"valid":true})
    );
    assert_eq!(
        call(&mut scope, &mut snapshots, &repo, "invalid")
            .unwrap_err()
            .code,
        "invalid_request"
    );
    assert_eq!(
        call(&mut scope, &mut snapshots, &outside, &token)
            .unwrap_err()
            .code,
        "outside_workspace"
    );
    fs::rename(repo.join(".git"), temporary.path().join("old-gitdir")).unwrap();
    Repository::init(&repo).unwrap();
    assert_eq!(
        call(&mut scope, &mut snapshots, &repo, &token).unwrap(),
        json!({"valid":false})
    );
}

#[test]
fn captured_root_identity_blocks_replacement_before_worker_init_and_mutation() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    Repository::init(&repo_path).unwrap();
    fs::write(repo_path.join("file"), "original").unwrap();
    let config = Config::default();
    let options = options(&config);
    let mut main_scope = None;
    let mut main_snapshots = Snapshots::default();
    let initialize = || {
        request(
            "initialize",
            json!({"version": 1, "workspace_roots": [workspace]}),
        )
    };
    assert_eq!(
        process(&mut main_scope, &mut main_snapshots, initialize(), &options).unwrap(),
        json!({"version": 1})
    );
    let captured = process(
        &mut main_scope,
        &mut main_snapshots,
        request("workspace_root_identities", json!({})),
        &options,
    )
    .unwrap();
    let token = captured["roots"][0]["identity"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(token.len(), 64);

    let mut worker_scope = None;
    let mut worker_snapshots = Snapshots::default();
    let strict = || {
        request(
            "initialize",
            json!({"version": 1, "workspace_roots": [workspace], "workspace_root_identities": [token]}),
        )
    };
    process(&mut worker_scope, &mut worker_snapshots, strict(), &options).unwrap();

    let saved = temporary.path().join("saved-workspace");
    fs::rename(&workspace, &saved).unwrap();
    fs::create_dir_all(&repo_path).unwrap();
    let replacement = Repository::init(&repo_path).unwrap();
    fs::write(repo_path.join("file"), "replacement").unwrap();
    let mut late_scope = None;
    let mut late_snapshots = Snapshots::default();
    assert_eq!(
        process(&mut late_scope, &mut late_snapshots, strict(), &options)
            .unwrap_err()
            .code,
        "stale_target"
    );
    assert_eq!(
        process(
            &mut main_scope,
            &mut main_snapshots,
            request("workspace_root_identities", json!({})),
            &options
        )
        .unwrap_err()
        .code,
        "stale_target"
    );
    let error = process_mutation(
        worker_scope.as_ref().unwrap(),
        request(
            "file_action",
            json!({
                "repo": repo_path, "path": "file", "operation": "stage"
            }),
        ),
    )
    .unwrap_err();
    assert_eq!(error.code, "outside_workspace");
    assert!(
        replacement
            .index()
            .unwrap()
            .get_path(std::path::Path::new("file"), 0)
            .is_none()
    );
    drop(replacement);

    fs::rename(&workspace, temporary.path().join("replacement-workspace")).unwrap();
    fs::rename(&saved, &workspace).unwrap();
    assert_eq!(
        process(&mut late_scope, &mut late_snapshots, strict(), &options).unwrap(),
        json!({"version": 1})
    );
}

#[test]
fn initialization_rejects_invalid_root_identity_payloads() {
    let temp = TempDir::new().unwrap();
    let mut scope = None;
    let mut snapshots = Snapshots::default();
    let config = Config::default();
    let options = options(&config);
    for identities in [
        json!([]),
        json!(["not-an-identity"]),
        json!(["0".repeat(64), "0".repeat(64)]),
    ] {
        let error = process(&mut scope, &mut snapshots, request("initialize", json!({
            "version": 1, "workspace_roots": [temp.path()], "workspace_root_identities": identities
        })), &options).unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }
}

#[cfg(unix)]
#[test]
fn replaced_workspace_symlink_is_never_authorized() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo = workspace.join("repo");
    fs::create_dir_all(&repo).unwrap();
    Repository::init(&repo).unwrap();
    let scope = super::Scope::new(vec![workspace.clone()]).unwrap();
    let token = scope.identities().unwrap()["roots"][0]["identity"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::rename(&workspace, temporary.path().join("saved")).unwrap();
    let outside = temporary.path().join("outside");
    fs::create_dir_all(outside.join("repo")).unwrap();
    Repository::init(outside.join("repo")).unwrap();
    std::os::unix::fs::symlink(&outside, &workspace).unwrap();
    assert!(!scope.allows(&repo));
    assert_eq!(
        super::Scope::new_with_identities(vec![workspace], Some(vec![token]), None)
            .err()
            .unwrap()
            .code,
        "stale_target"
    );
}

#[cfg(unix)]
#[test]
fn source_symlink_retarget_does_not_authorize_original_root_or_replacement() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().unwrap();
    let original = temporary.path().join("original");
    let outside = temporary.path().join("outside");
    let source = temporary.path().join("workspace-link");
    fs::create_dir_all(original.join("repo")).unwrap();
    fs::create_dir_all(outside.join("repo")).unwrap();
    let repo = Repository::init(original.join("repo")).unwrap();
    Repository::init(outside.join("repo")).unwrap();
    fs::write(original.join("repo/file"), "original").unwrap();
    symlink(&original, &source).unwrap();
    let config = Config::default();
    let options = options(&config);
    let mut scope = None;
    let mut snapshots = Snapshots::default();
    process(
        &mut scope,
        &mut snapshots,
        request(
            "initialize",
            json!({
                "version": 1, "workspace_roots": [original], "workspace_root_sources": [source]
            }),
        ),
        &options,
    )
    .unwrap();
    let handshake = process(
        &mut scope,
        &mut snapshots,
        request("workspace_root_identities", json!({})),
        &options,
    )
    .unwrap();
    let identity = handshake["roots"][0]["identity"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(handshake["roots"][0]["source"], json!(source));
    fs::remove_file(&source).unwrap();
    symlink(&outside, &source).unwrap();
    let result = process_mutation(
        scope.as_ref().unwrap(),
        request(
            "file_action",
            json!({
                "repo": original.join("repo"), "path": "file", "operation": "stage"
            }),
        ),
    );
    assert_eq!(result.unwrap_err().code, "outside_workspace");
    assert!(
        repo.index()
            .unwrap()
            .get_path(std::path::Path::new("file"), 0)
            .is_none()
    );
    let mut late_scope = None;
    let mut late_snapshots = Snapshots::default();
    let error = process(
        &mut late_scope,
        &mut late_snapshots,
        request(
            "initialize",
            json!({
                "version": 1, "workspace_roots": [original], "workspace_root_sources": [source],
                "workspace_root_identities": [identity]
            }),
        ),
        &options,
    )
    .unwrap_err();
    assert_eq!(error.code, "stale_target");
    fs::remove_file(&source).unwrap();
    symlink(&original, &source).unwrap();
    assert!(scope.as_ref().unwrap().allows(&original.join("repo")));
}

#[cfg(unix)]
#[test]
fn duplicate_canonical_roots_keep_independent_authorized_sources() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("root");
    let other = temporary.path().join("other");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::create_dir_all(root.join("repo")).unwrap();
    fs::create_dir(&other).unwrap();
    Repository::init(root.join("repo")).unwrap();
    symlink(&root, &first).unwrap();
    symlink(&root, &second).unwrap();
    let scope = super::Scope::new_with_identities(
        vec![root.clone(), root.clone()],
        None,
        Some(vec![first.clone(), second.clone()]),
    )
    .unwrap();
    let roots = scope.identities().unwrap()["roots"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0]["source"], json!(first));
    assert_eq!(roots[1]["source"], json!(second));
    fs::remove_file(&first).unwrap();
    symlink(&other, &first).unwrap();
    assert!(scope.allows(&root.join("repo")));
    assert_eq!(scope.identities().unwrap_err().code, "stale_target");
    let valid = super::Scope::new_with_identities(
        vec![root.clone()],
        Some(vec![roots[1]["identity"].as_str().unwrap().to_owned()]),
        Some(vec![second]),
    )
    .unwrap();
    assert!(valid.allows(&root.join("repo")));
}
