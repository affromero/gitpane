#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn wait_for_file(path: &std::path::Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "launcher worker exited before probe started"
        );
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("launcher probe did not start");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn check_probe_cancellation(signal: bool) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git2::Repository::init(&repo).unwrap();
    let config = root.join("config.toml");
    std::fs::write(
        &config,
        "[open]\ncommand = 'printf test'\nplacement = 'ask'\n",
    )
    .unwrap();
    let pid_file = root.join("probe.pid");
    let descendant_file = root.join("descendant.pid");
    let script = root.join("tmux");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep 30 &\nprintf '%s' $! > '{}'\nprintf '%s' $$ > '{}'\nwait\n",
            descendant_file.display(),
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let search = std::env::join_paths(std::iter::once(root.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_gitpane");
    let initial = [
        json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[repo]}}),
        json!({"id":2,"method":"launchers_list","params":{"repo":repo}}),
    ]
    .iter()
    .map(|value| format!("{value}\n"))
    .collect::<String>();
    let mut bridge = Command::new(binary)
        .arg("serve")
        .env("GITPANE_CONFIG", &config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    bridge
        .stdin
        .take()
        .unwrap()
        .write_all(initial.as_bytes())
        .unwrap();
    let output = bridge.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let fingerprint = replies[1]["result"]["actions"][0]["fingerprint"]
        .as_str()
        .unwrap();
    let mut child = Command::new(binary)
        .args(["serve", "--liveness-worker"])
        .env("GITPANE_CONFIG", &config)
        .env("PATH", search)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        stdin,
        "{}",
        json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[repo]}})
    )
    .unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let initialized: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        initialized["result"]["capabilities"]["launcher_choices_worker"],
        true
    );
    writeln!(stdin, "{}", json!({"id":2,"method":"launcher_choices","params":{"repo":repo,"id":"open","fingerprint":fingerprint,"backend":"tmux"}})).unwrap();
    wait_for_file(&pid_file, &mut child);
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let descendant: i32 = std::fs::read_to_string(&descendant_file)
        .unwrap()
        .parse()
        .unwrap();
    stdin.write_all(b"unread").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "temporary lack of input cancelled the probe"
    );
    if signal {
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    } else {
        drop(stdin);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("launcher worker did not cancel its probe after parent input closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "launcher probe survived cancellation"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(descendant, 0) } == 0 && Instant::now() < deadline {
        #[cfg(target_os = "linux")]
        if std::fs::read_to_string(format!("/proc/{descendant}/stat"))
            .is_ok_and(|stat| stat.contains(") Z "))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(
        unsafe { libc::kill(descendant, 0) },
        0,
        "launcher probe descendant survived cancellation"
    );
}

#[test]
fn launcher_choice_worker_cancels_probe_when_parent_input_closes() {
    check_probe_cancellation(false);
}

#[test]
fn launcher_choice_worker_cancels_probe_on_sigterm() {
    check_probe_cancellation(true);
}
