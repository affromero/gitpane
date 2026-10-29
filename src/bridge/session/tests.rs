use super::*;
use std::fs;
use std::path::Path;

fn repo(path: &Path) {
    let repository = git2::Repository::init(path).unwrap();
    let mut index = repository.index().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repository
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
}

fn request(repos: Vec<PathBuf>) -> LivenessRequest {
    LivenessRequest {
        repos,
        backend: LivenessBackend::Auto,
    }
}

#[test]
fn dedicated_worker_advertises_capability_and_rejects_other_queries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut config = crate::config::Config::default();
    config.ui.show_liveness = false;
    let options = super::super::BridgeOptions {
        submodules: &config.submodules,
        graph: &config.graph,
        worktree: &config.worktree,
        review_base: config.review.base.as_deref(),
        theme: &config,
    };
    let requests = [
        json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[root]}}),
        json!({"id":2,"method":"theme","params":{}}),
        json!({"id":3,"method":"session_liveness","params":{"repos":[]}}),
    ];
    let input = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect::<String>();
    let mut output = Vec::new();
    super::super::serve_inner(
        &mut std::io::Cursor::new(input),
        &mut output,
        &options,
        false,
        true,
    )
    .unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        replies[0]["result"]["capabilities"]["liveness_worker"],
        true
    );
    assert_eq!(replies[1]["error"]["code"], "invalid_request");
    assert_eq!(replies[2]["result"]["reason"], "disabled");
}

#[test]
fn explicit_backend_works_without_a_host_environment_and_honors_preference() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    repo(&root);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let explicit = || LivenessRequest {
        repos: vec![root.clone()],
        backend: LivenessBackend::Tmux,
    };
    let result = liveness_with(
        &scope,
        explicit(),
        true,
        || panic!("explicit backend should not inspect the environment"),
        |kind| {
            assert_eq!(kind, "tmux");
            Ok(vec![("session".into(), root.clone())])
        },
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"enabled":true,"reason":null,"repos":[{"repo":root,"live":true}]})
    );
    let disabled = liveness_with(
        &scope,
        explicit(),
        false,
        || panic!("disabled"),
        |_| panic!("disabled"),
    )
    .unwrap();
    assert_eq!(
        disabled,
        json!({"enabled":false,"reason":"disabled","repos":[]})
    );
    let invalid =
        serde_json::from_value::<LivenessRequest>(json!({"repos": [], "backend": "other"}));
    assert!(invalid.is_err());
}

#[test]
fn liveness_matches_nested_panes_and_linked_worktrees() {
    if !crate::git::git_test_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let main = root.join("main");
    let linked = root.join("linked");
    let sibling = root.join("other");
    repo(&main);
    repo(&sibling);
    let output = crate::git::process::git_command(&main)
        .args(["worktree", "add", "-b", "linked"])
        .arg(&linked)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::create_dir(linked.join("nested")).unwrap();
    let scope = Scope::new(vec![root]).unwrap();
    let result = liveness_with(
        &scope,
        request(vec![main.clone(), linked.clone(), sibling.clone()]),
        true,
        || Multiplexer::Tmux,
        |kind| {
            assert_eq!(kind, "tmux");
            Ok(vec![("session".into(), linked.join("nested"))])
        },
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"enabled":true,"reason":null,"repos":[
            {"repo":main,"live":false},{"repo":linked,"live":true},{"repo":sibling,"live":false}
        ]})
    );
}

#[test]
fn disabled_and_plain_host_do_not_probe() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    repo(&root);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let disabled = liveness_with(
        &scope,
        request(vec![root.clone()]),
        false,
        || panic!("disabled preference must not detect a host"),
        |_| panic!("must not probe"),
    )
    .unwrap();
    assert_eq!(
        disabled,
        json!({"enabled":false,"reason":"disabled","repos":[]})
    );
    let unsupported = liveness_with(
        &scope,
        request(vec![root]),
        true,
        || Multiplexer::None,
        |_| panic!("must not probe"),
    )
    .unwrap();
    assert_eq!(
        unsupported,
        json!({"enabled":false,"reason":"unsupported","repos":[]})
    );
}

#[test]
fn selected_backend_failure_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    repo(&root);
    let scope = Scope::new(vec![root.clone()]).unwrap();
    let error = liveness_with(
        &scope,
        LivenessRequest {
            repos: vec![root],
            backend: LivenessBackend::Herdr,
        },
        true,
        || panic!("explicit backend should not detect"),
        |kind| {
            Err(BridgeError::new(
                "session_error",
                format!("{kind} unavailable"),
            ))
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "session_error");
    assert!(error.message.contains("herdr"));
}

#[test]
fn scope_and_repository_replacement_are_checked_around_probe() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    repo(&workspace);
    repo(&outside);
    let scope = Scope::new(vec![workspace.clone()]).unwrap();
    let error = liveness_with(
        &scope,
        request(vec![outside]),
        false,
        || Multiplexer::None,
        |_| panic!("must not probe"),
    )
    .unwrap_err();
    assert_eq!(error.code, "outside_workspace");
    let error = liveness_with(
        &scope,
        request(vec![workspace.clone()]),
        true,
        || Multiplexer::Tmux,
        |_| {
            fs::rename(workspace.join(".git"), workspace.join(".old-git")).unwrap();
            git2::Repository::init(&workspace).unwrap();
            Ok(Vec::new())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "stale_target");
}

#[test]
fn oversized_batch_is_rejected() {
    let scope = Scope::default();
    let error = liveness_with(
        &scope,
        request(vec![
            PathBuf::from("/unused");
            MAX_LIVENESS_REPOSITORIES + 1
        ]),
        true,
        || Multiplexer::Tmux,
        |_| panic!("must not probe"),
    )
    .unwrap_err();
    assert_eq!(error.code, "invalid_request");
}
