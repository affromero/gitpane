#![cfg(unix)]

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn git_executable() -> Option<PathBuf> {
    match Command::new("git").arg("--version").output() {
        Ok(output) => assert!(output.status.success(), "Git is installed but broken"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Skipping content worker test: Git is unavailable");
            return None;
        }
        Err(error) => panic!("Cannot inspect Git: {error}"),
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .map(|path| path.canonicalize().unwrap())
}

struct Fixture {
    temporary: tempfile::TempDir,
    git: PathBuf,
    oid: String,
    root_identity: String,
    repo_identity: String,
}

impl Fixture {
    fn new(contents: &[u8]) -> Option<Self> {
        let git = git_executable()?;
        let temporary = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temporary.path()).unwrap();
        std::fs::write(temporary.path().join("data"), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("data")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap()
            .to_string();
        let mut bridge = Command::new(env!("CARGO_BIN_EXE_gitpane"))
            .arg("serve")
            .env("GITPANE_CONFIG", temporary.path().join("config.toml"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = bridge.stdin.take().unwrap();
        for request in [
            json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[temporary.path()]}}),
            json!({"id":2,"method":"workspace_root_identities","params":{}}),
            json!({"id":3,"method":"commit_files","params":{"repo":temporary.path(),"oid":oid}}),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
        drop(stdin);
        let output = bridge.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let replies: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let root_identity = replies[1]["result"]["roots"][0]["identity"]
            .as_str()
            .unwrap()
            .to_owned();
        let repo_identity = replies[2]["result"]["repo_identity"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(tree);
        drop(repo);
        Some(Self {
            temporary,
            git,
            oid,
            root_identity,
            repo_identity,
        })
    }

    fn request(&self) -> Value {
        json!({"version":1,"workspace_roots":[self.temporary.path()],
            "workspace_root_identities":[self.root_identity],"workspace_root_sources":[self.temporary.path()],
            "repo":self.temporary.path(),"expected_repo_identity":self.repo_identity,
            "git_executable":self.git,"operation":{"kind":"content","ref":self.oid,"path":"data"}})
    }

    fn start(&self, request: &Value) -> (Child, ChildStdin) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gitpane"))
            .args(["serve", "--content-worker"])
            .process_group(0)
            .env("GITPANE_CONFIG", self.temporary.path().join("config.toml"))
            .env("GIT_DIR", "/does/not/exist")
            .env("GIT_WORK_TREE", "/does/not/exist")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{request}").unwrap();
        (child, stdin)
    }
}

#[test]
fn content_worker_returns_binary_from_scoped_repository() {
    let bytes = vec![0, 255, 10, 42, 0];
    let Some(fixture) = Fixture::new(&bytes) else {
        return;
    };
    let (child, stdin) = fixture.start(&fixture.request());
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, bytes);

    let mut request = fixture.request();
    request["expected_repo_identity"] = json!("a".repeat(64));
    let (child, stdin) = fixture.start(&request);
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stale_target"));
}

#[test]
fn cancelled_worker_kills_converter_that_ignores_termination() {
    let Some(fixture) = Fixture::new(b"text\n") else {
        return;
    };
    let pid_file = fixture.temporary.path().join("converter.pid");
    let script = fixture.temporary.path().join("converter.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ntrap '' TERM\necho $$ > '{}'\nwhile :; do sleep 1; done\n",
            pid_file.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        fixture.temporary.path().join(".gitattributes"),
        "data diff=slow\n",
    )
    .unwrap();
    let repo = git2::Repository::open(fixture.temporary.path()).unwrap();
    repo.config()
        .unwrap()
        .set_str("diff.slow.textconv", script.to_str().unwrap())
        .unwrap();
    let (child, stdin) = fixture.start(&fixture.request());
    let started = Instant::now();
    while !pid_file.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pid_file.exists(), "converter did not start");
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let started = Instant::now();
    while unsafe { libc::kill(pid, 0) } == 0 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "converter survived cancellation"
    );
}

#[test]
fn worker_cancels_when_stdout_is_blocked() {
    use std::os::fd::AsRawFd;
    let Some(fixture) = Fixture::new(&vec![42; 2 * 1024 * 1024]) else {
        return;
    };
    let (mut child, stdin) = fixture.start(&fixture.request());
    let mut pollfd = libc::pollfd {
        fd: child.stdout.as_ref().unwrap().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&mut pollfd, 1, 5000) },
        1,
        "worker produced no output"
    );
    drop(stdin);
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() >= Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("worker did not cancel while stdout was blocked");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn successful_read_closes_pipe_held_by_ignoring_descendant() {
    let Some(fixture) = Fixture::new(b"text\n") else {
        return;
    };
    let script = fixture.temporary.path().join("fake-git.sh");
    let ready = fixture.temporary.path().join("descendant-ready");
    std::fs::write(&script, format!(
        "#!/bin/sh\n(trap '' TERM; echo ready > '{}'; while :; do sleep 1; done) 2>/dev/null &\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf scoped-bytes\n",
        ready.display(), ready.display()
    )).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut request = fixture.request();
    request["git_executable"] = json!(script);
    let (child, stdin) = fixture.start(&request);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let result = receiver.recv_timeout(Duration::from_secs(5));
    drop(stdin);
    let output = result
        .expect("successful worker left a descendant holding stdout")
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"scoped-bytes");
}

#[test]
fn incomplete_gate_cannot_start_git() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpane"))
        .args(["serve", "--content-exec"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"GO {").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

fn update_fixture(fixture: &Fixture, commit: bool) {
    std::fs::write(fixture.temporary.path().join("data"), b"changed content\n").unwrap();
    let repo = git2::Repository::open(fixture.temporary.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("data")).unwrap();
    index.write().unwrap();
    if commit {
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "changed",
            &tree,
            &[&parent],
        )
        .unwrap();
    }
}

fn wait_for_file(path: &Path, child: &mut Child) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "worker exited before the fixture was ready"
        );
        if started.elapsed() >= Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("worker fixture did not become ready");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn verified_content_rejects_index_and_head_changes_during_conversion() {
    use std::os::unix::fs::PermissionsExt;
    for reference in [":0", "HEAD"] {
        let Some(fixture) = Fixture::new(b"original\n") else {
            return;
        };
        let script = fixture.temporary.path().join("converter.sh");
        let ready = fixture.temporary.path().join("ready");
        let release = fixture.temporary.path().join("release");
        std::fs::write(&script, format!(
            "#!/bin/sh\necho ready > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\ncat \"$1\"\n",
            ready.display(), release.display()
        )).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            fixture.temporary.path().join(".gitattributes"),
            "data diff=held\n",
        )
        .unwrap();
        let repo = git2::Repository::open(fixture.temporary.path()).unwrap();
        repo.config()
            .unwrap()
            .set_str("diff.held.textconv", script.to_str().unwrap())
            .unwrap();
        let mut request = fixture.request();
        request["operation"] = json!({"kind":"verified_content","ref":reference,"path":"data"});
        let (mut child, stdin) = fixture.start(&request);
        wait_for_file(&ready, &mut child);
        update_fixture(&fixture, reference == "HEAD");
        std::fs::write(&release, "release").unwrap();
        let output = child.wait_with_output().unwrap();
        drop(stdin);
        assert!(!output.status.success(), "changed {reference} was accepted");
        assert!(String::from_utf8_lossy(&output.stderr).contains("stale_target"));
    }
}

#[test]
fn verified_content_waits_for_inherited_stdout_before_post_validation() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fixture) = Fixture::new(b"original\n") else {
        return;
    };
    let script = fixture.temporary.path().join("git-wrapper.sh");
    let ready = fixture.temporary.path().join("ready");
    let release = fixture.temporary.path().join("release");
    std::fs::write(&script, format!(
        "#!/bin/sh\ncase \"$*\" in\n*' show --textconv '*) (echo ready > '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; printf late) 2>/dev/null & printf early; exit 0;;\nesac\nexec '{}' \"$@\"\n",
        ready.display(), release.display(), fixture.git.display()
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut request = fixture.request();
    request["git_executable"] = json!(script);
    request["operation"] = json!({"kind":"verified_content","ref":":0","path":"data"});
    let (mut child, stdin) = fixture.start(&request);
    wait_for_file(&ready, &mut child);
    assert!(
        child.try_wait().unwrap().is_none(),
        "worker accepted content before stdout EOF"
    );
    update_fixture(&fixture, false);
    std::fs::write(&release, "release").unwrap();
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(
        !output.status.success(),
        "content was accepted after index mutation"
    );
    assert_eq!(output.stdout, b"earlylate");
    assert!(String::from_utf8_lossy(&output.stderr).contains("stale_target"));
}

#[test]
fn verified_content_returns_binary_and_rejects_partial_failed_output() {
    use std::os::unix::fs::PermissionsExt;
    let bytes = [0, 255, 10, 42, 0];
    let Some(fixture) = Fixture::new(&bytes) else {
        return;
    };
    let mut request = fixture.request();
    request["operation"] = json!({"kind":"verified_content","ref":"HEAD","path":"data"});
    let (child, stdin) = fixture.start(&request);
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, bytes);

    let script = fixture.temporary.path().join("git-wrapper.sh");
    std::fs::write(&script, format!(
        "#!/bin/sh\ncase \"$*\" in\n*' show --textconv '*) printf partial; i=0; while [ \"$i\" -lt 1500 ]; do printf '%080d\\n' \"$i\" >&2; i=$((i+1)); done; exit 17;;\nesac\nexec '{}' \"$@\"\n",
        fixture.git.display()
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    request["git_executable"] = json!(script);
    let (child, stdin) = fixture.start(&request);
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"partial");
    assert!(String::from_utf8_lossy(&output.stderr).contains("repository_error"));
}
