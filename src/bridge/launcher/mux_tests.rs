use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

use super::{ChoicesRequest, ExecuteRequest, choices, execute};
use crate::bridge::launcher::{actions, load};
use crate::bridge::scope::Scope;

const CHILD: &str = "GITPANE_LAUNCHER_TEST_CHILD";

fn child() -> Option<(tempfile::TempDir, UnixListener)> {
    if std::env::var_os(CHILD).is_some() {
        return None;
    }
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("mux.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    Some((directory, listener))
}

fn run_child(name: &str, root: &Path, backend: &str) {
    let path = std::env::join_paths(std::iter::once(root.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .env("GITPANE_LAUNCHER_TEST_ROOT", root)
        .env("GITPANE_CONFIG", root.join("config.toml"))
        .env("PATH", path)
        .env("HERDR_SOCKET_PATH", root.join("mux.sock"))
        .env("HERDR_PANE_ID", "w1:p1")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("GITPANE_LAUNCHER_TEST_BACKEND", backend)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_script(root: &Path, program: &str, source: &str) {
    let path = root.join(program);
    fs::write(&path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn fixture() -> (std::path::PathBuf, Scope) {
    let root = std::path::PathBuf::from(std::env::var_os("GITPANE_LAUNCHER_TEST_ROOT").unwrap());
    let repo = root.join("repo $(touch unsafe)");
    fs::create_dir(&repo).unwrap();
    git2::Repository::init(&repo).unwrap();
    let scope = Scope::new(vec![repo.clone()]).unwrap();
    (repo, scope)
}

#[test]
fn tmux_alias_launch_is_pinned_and_rejects_changed_target_or_config() {
    if let Some((directory, _listener)) = child() {
        let root = directory.path();
        fs::write(
            root.join("config.toml"),
            "[open]\ncommand = 'printf %s {path}'\nplacement = 'split-window -h -t agents'\n",
        )
        .unwrap();
        fs::write(root.join("pane"), "%7").unwrap();
        write_script(
            root,
            "tmux",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\nlist-panes|display-message) read -r pane < '{0}/pane'; printf '%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' 4321 '{0}/mux.sock' '$2' '@3' \"$pane\" 9876 'agents:0.0 editor';;\nsplit-window|new-window) printf '%s\\n' \"$@\" > '{0}/launched';;\n*) exit 1;;\nesac\n",
                root.display()
            ),
        );
        run_child(
            "bridge::launcher::mux::tests::tmux_alias_launch_is_pinned_and_rejects_changed_target_or_config",
            root,
            "tmux",
        );
        return;
    }
    let (repo, scope) = fixture();
    let root = repo.parent().unwrap();
    let action = actions(&load().unwrap()).remove(0);
    let fp = action.fingerprint();
    let collect = || {
        choices(
            &scope,
            ChoicesRequest {
                expected_base_ref: None,
                repo: repo.clone(),
                id: "open".into(),
                fingerprint: fp.clone(),
                backend: "tmux".into(),
            },
        )
        .unwrap()
    };
    let selection = collect();
    let identity = selection["repo_identity"].as_str().unwrap().to_owned();
    let token = selection["choices"][0]["context_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = || ExecuteRequest {
        expected_base_ref: None,
        repo: repo.clone(),
        id: "open".into(),
        fingerprint: fp.clone(),
        expected_repo_identity: identity.clone(),
        backend: "tmux".into(),
        choice: "configured".into(),
        context_token: token.clone(),
    };
    assert_eq!(execute(&scope, request()).unwrap()["launched"], true);
    let argv = fs::read_to_string(root.join("launched")).unwrap();
    assert!(argv.contains("\n-t\n%7\n"), "{argv}");
    assert!(argv.contains(repo.to_str().unwrap()), "{argv}");
    assert!(!root.join("unsafe").exists());
    fs::remove_file(root.join("launched")).unwrap();
    fs::write(root.join("pane"), "%8").unwrap();
    assert_eq!(execute(&scope, request()).unwrap_err().code, "stale_target");
    assert!(!root.join("launched").exists());

    fs::write(
        root.join("config.toml"),
        "[review]\ncommand = 'printf %s {base} {path}'\nplacement = 'split-window -h -t agents'\n",
    )
    .unwrap();
    let repository = git2::Repository::open(&repo).unwrap();
    let tree_id = repository.treebuilder(None).unwrap().write().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let signature = git2::Signature::now("Gitpane Test", "test@example.invalid").unwrap();
    let commit = repository
        .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    for branch in ["main", "topic"] {
        repository
            .reference(
                &format!("refs/remotes/origin/{branch}"),
                commit,
                true,
                "test",
            )
            .unwrap();
    }
    repository
        .reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "test",
        )
        .unwrap();
    let action = actions(&load().unwrap()).remove(1);
    let selection = choices(
        &scope,
        ChoicesRequest {
            repo: repo.clone(),
            id: "review".into(),
            fingerprint: action.fingerprint(),
            backend: "tmux".into(),
            expected_base_ref: Some("origin/main".into()),
        },
    )
    .unwrap();
    let review_request = || ExecuteRequest {
        repo: repo.clone(),
        id: "review".into(),
        fingerprint: action.fingerprint(),
        backend: "tmux".into(),
        expected_base_ref: Some("origin/main".into()),
        expected_repo_identity: selection["repo_identity"].as_str().unwrap().into(),
        choice: "configured".into(),
        context_token: selection["choices"][0]["context_token"]
            .as_str()
            .unwrap()
            .into(),
    };
    assert_eq!(execute(&scope, review_request()).unwrap()["launched"], true);
    assert!(
        fs::read_to_string(root.join("launched"))
            .unwrap()
            .contains("origin/main")
    );
    fs::remove_file(root.join("launched")).unwrap();
    repository
        .reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/topic",
            true,
            "test",
        )
        .unwrap();
    assert_eq!(
        execute(&scope, review_request()).unwrap_err().code,
        "stale_launcher"
    );
    assert!(!root.join("launched").exists());
    fs::write(root.join("pane"), "%7").unwrap();
    fs::write(
        root.join("config.toml"),
        "[open]\ncommand = 'other {path}'\nplacement = 'split-window -h -t agents'\n",
    )
    .unwrap();
    assert_eq!(
        execute(&scope, request()).unwrap_err().code,
        "stale_launcher"
    );
    assert!(!root.join("launched").exists());
}

#[test]
fn herdr_partial_failure_reports_created_pane() {
    if let Some((directory, _listener)) = child() {
        let root = directory.path();
        fs::write(
            root.join("config.toml"),
            "[open]\ncommand = 'printf %s {path}'\nplacement = 'split-window -h'\n",
        )
        .unwrap();
        write_script(
            root,
            "herdr",
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'pane list') printf '%s' '{{\"result\":{{\"panes\":[{{\"pane_id\":\"w1:p1\",\"terminal_id\":\"term1\",\"workspace_id\":\"w1\"}}]}}}}';;\n'pane split') printf '%s\\n' \"$@\" > '{0}/created'; printf '%s' '{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p2\"}}}}}}';;\n'pane run') echo failed >&2; exit 3;;\n*) exit 1;;\nesac\n",
                root.display()
            ),
        );
        run_child(
            "bridge::launcher::mux::tests::herdr_partial_failure_reports_created_pane",
            root,
            "herdr",
        );
        return;
    }
    let (repo, scope) = fixture();
    let root = repo.parent().unwrap();
    let action = actions(&load().unwrap()).remove(0);
    let fp = action.fingerprint();
    let result = choices(
        &scope,
        ChoicesRequest {
            expected_base_ref: None,
            repo: repo.clone(),
            id: "open".into(),
            fingerprint: fp.clone(),
            backend: "herdr".into(),
        },
    )
    .unwrap();
    let request = ExecuteRequest {
        expected_base_ref: None,
        repo: repo.clone(),
        id: "open".into(),
        fingerprint: fp,
        expected_repo_identity: result["repo_identity"].as_str().unwrap().into(),
        backend: "herdr".into(),
        choice: "configured".into(),
        context_token: result["choices"][0]["context_token"]
            .as_str()
            .unwrap()
            .into(),
    };
    let error = execute(&scope, request).unwrap_err();
    assert_eq!(error.code, "partial_completion");
    assert!(error.message.contains("pane was created"));
    assert!(
        fs::read_to_string(root.join("created"))
            .unwrap()
            .contains("\n--pane\nw1:p1\n")
    );
}

#[test]
fn herdr_new_tab_is_pinned_to_verified_workspace_and_rejects_stale_context() {
    if let Some((directory, _listener)) = child() {
        let root = directory.path();
        fs::write(
            root.join("config.toml"),
            "[open]\ncommand = 'printf %s {path}'\nplacement = 'ask'\n",
        )
        .unwrap();
        fs::write(root.join("workspace"), "w1\n").unwrap();
        fs::write(root.join("terminal"), "term1\n").unwrap();
        fs::write(root.join("returned-workspace"), "w1\n").unwrap();
        write_script(
            root,
            "herdr",
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'pane list') read -r workspace < '{0}/workspace'; read -r terminal < '{0}/terminal'; printf '{{\"result\":{{\"panes\":[{{\"pane_id\":\"w1:p1\",\"terminal_id\":\"%s\",\"workspace_id\":\"%s\",\"tab_id\":\"w1:t1\"}}' \"$terminal\" \"$workspace\"; if [ -f '{0}/created' ]; then printf ',{{\"pane_id\":\"w1:p2\",\"terminal_id\":\"term2\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t2\"}}'; fi; printf ']}}}}';;\n'tab create') printf '%s\\n' \"$@\" > '{0}/created'; if [ -f '{0}/malformed' ]; then printf invalid; elif [ -f '{0}/existing' ]; then printf '{{\"result\":{{\"type\":\"tab_created\",\"tab\":{{\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\"}},\"root_pane\":{{\"pane_id\":\"w1:p1\",\"terminal_id\":\"term1\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\"}}}}}}'; else read -r workspace < '{0}/returned-workspace'; printf '{{\"result\":{{\"type\":\"tab_created\",\"tab\":{{\"workspace_id\":\"%s\",\"tab_id\":\"w1:t2\"}},\"root_pane\":{{\"pane_id\":\"w1:p2\",\"terminal_id\":\"term2\",\"workspace_id\":\"%s\",\"tab_id\":\"w1:t2\"}}}}}}' \"$workspace\" \"$workspace\"; fi;;\n'pane run') printf '%s\\n' \"$@\" > '{0}/ran';;\n*) exit 1;;\nesac\n",
                root.display()
            ),
        );
        run_child(
            "bridge::launcher::mux::tests::herdr_new_tab_is_pinned_to_verified_workspace_and_rejects_stale_context",
            root,
            "herdr",
        );
        return;
    }
    let (repo, scope) = fixture();
    let root = repo.parent().unwrap();
    let action = actions(&load().unwrap()).remove(0);
    let fingerprint = action.fingerprint();
    let choose = || {
        choices(
            &scope,
            ChoicesRequest {
                expected_base_ref: None,
                repo: repo.clone(),
                id: "open".into(),
                fingerprint: fingerprint.clone(),
                backend: "herdr".into(),
            },
        )
        .unwrap()
    };
    let first = choose();
    let choice = first["choices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["id"] == "new-tab")
        .unwrap();
    let request = || ExecuteRequest {
        expected_base_ref: None,
        repo: repo.clone(),
        id: "open".into(),
        fingerprint: fingerprint.clone(),
        expected_repo_identity: first["repo_identity"].as_str().unwrap().into(),
        backend: "herdr".into(),
        choice: "new-tab".into(),
        context_token: choice["context_token"].as_str().unwrap().into(),
    };
    fs::write(root.join("workspace"), "w2\n").unwrap();
    assert_eq!(execute(&scope, request()).unwrap_err().code, "stale_target");
    assert!(!root.join("created").exists());
    fs::write(root.join("workspace"), "w1\n").unwrap();
    fs::write(root.join("terminal"), "other\n").unwrap();
    assert_eq!(execute(&scope, request()).unwrap_err().code, "stale_target");
    assert!(!root.join("created").exists());
    fs::write(root.join("terminal"), "term1\n").unwrap();
    assert_eq!(execute(&scope, request()).unwrap()["launched"], true);
    let argv = fs::read_to_string(root.join("created")).unwrap();
    assert!(argv.contains("\n--workspace\nw1\n"), "{argv}");
    assert!(argv.contains(repo.to_str().unwrap()), "{argv}");
    assert!(root.join("ran").exists());
    assert!(!root.join("unsafe").exists());
    fs::remove_file(root.join("created")).unwrap();
    fs::remove_file(root.join("ran")).unwrap();
    fs::write(root.join("returned-workspace"), "w2\n").unwrap();
    assert_eq!(
        execute(&scope, request()).unwrap_err().code,
        "partial_completion"
    );
    assert!(root.join("created").exists());
    assert!(!root.join("ran").exists());
    fs::remove_file(root.join("created")).unwrap();
    fs::write(root.join("malformed"), "yes").unwrap();
    assert_eq!(
        execute(&scope, request()).unwrap_err().code,
        "partial_completion"
    );
    assert!(!root.join("ran").exists());
    fs::remove_file(root.join("created")).unwrap();
    fs::remove_file(root.join("malformed")).unwrap();
    fs::write(root.join("returned-workspace"), "w1\n").unwrap();
    fs::write(root.join("existing"), "yes").unwrap();
    assert_eq!(
        execute(&scope, request()).unwrap_err().code,
        "partial_completion"
    );
    assert!(!root.join("ran").exists());
    fs::remove_file(root.join("created")).unwrap();
    fs::remove_file(root.join("existing")).unwrap();
    fs::remove_file(root.join("mux.sock")).unwrap();
    let _replacement = UnixListener::bind(root.join("mux.sock")).unwrap();
    assert_eq!(execute(&scope, request()).unwrap_err().code, "stale_target");
    assert!(!root.join("created").exists());
}
