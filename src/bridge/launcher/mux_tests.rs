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
fn herdr_partial_failure_reports_created_pane_and_unsupported_new_tab() {
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
            "bridge::launcher::mux::tests::herdr_partial_failure_reports_created_pane_and_unsupported_new_tab",
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
            repo: repo.clone(),
            id: "open".into(),
            fingerprint: fp.clone(),
            backend: "herdr".into(),
        },
    )
    .unwrap();
    let request = ExecuteRequest {
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
    fs::write(
        root.join("config.toml"),
        "[open]\ncommand = 'printf test'\nplacement = 'new-window'\n",
    )
    .unwrap();
    let action = actions(&load().unwrap()).remove(0);
    let error = choices(
        &scope,
        ChoicesRequest {
            repo,
            id: "open".into(),
            fingerprint: action.fingerprint(),
            backend: "herdr".into(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "unsupported_placement");
}
