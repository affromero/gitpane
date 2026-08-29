//! Tests for the process-group kill used by the fetch timeout path.
//!
//! `git fetch` spawns an `ssh` child for ssh remotes; killing only the git
//! process leaves that ssh orphaned with its server-side connection alive.
//! The fix spawns the fetch in its own process group and kills the group, so
//! this test asserts that a process-group kill takes grandchildren down too.
//!
//! `kill_in_flight_git_ops` is the quit-time counterpart: it kills every
//! registered killable git group (fetch, pull/push/submodule) so exiting the
//! app does not leave ssh running.
#![cfg(unix)]

use super::{
    ResetsShuttingDown, capture_with_timeout, kill_in_flight_git_ops, kill_process_group,
    register_killable_pid,
};

/// sh forks a `sleep` grandchild, prints its pid, then waits on it — the
/// same parent/grandchild shape as `git fetch` + `ssh`.
#[test]
fn process_group_kill_takes_grandchildren() {
    use std::io::BufRead;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut child = Command::new("sh")
        .arg("-c")
        // sleep inherits the stdout pipe but never writes to it; read_line
        // below returns at echo's newline, so reading the pid does not wait
        // for sleep to exit.
        .arg("sleep 60 2>/dev/null & echo $!; wait")
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().expect("sh stdout"))
        .read_line(&mut line)
        .expect("read grandchild pid");
    let grandchild: i32 = line.trim().parse().expect("grandchild pid");

    // Sanity: the grandchild is alive before the kill.
    assert_eq!(
        unsafe { libc::kill(grandchild, 0) },
        0,
        "grandchild should be alive before the kill"
    );

    kill_process_group(&mut child);
    let _ = child.wait();

    // SIGKILL is immediate, but the orphaned grandchild is reaped by init,
    // which may take a moment — poll for ESRCH with a generous bound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let rc = unsafe { libc::kill(grandchild, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "grandchild still alive or unreaped 5s after the process-group kill"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `kill_in_flight_git_ops` must take down every registered killable git process
/// group, so quitting the app cannot leave an orphaned ssh connection running
/// while a git operation waits out its timeout or network fetch.
#[test]
fn kill_in_flight_git_ops_takes_registered_groups_down() {
    use std::io::BufRead;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let _lock = super::TEST_KILL_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Reset the one-way global SHUTTING_DOWN flag on drop (even on panic);
    // declared after the lock so it is reset before the lock is released.
    let _reset = ResetsShuttingDown::new();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60 2>/dev/null & echo $!; wait")
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().expect("sh stdout"))
        .read_line(&mut line)
        .expect("read grandchild pid");
    let grandchild: i32 = line.trim().parse().expect("grandchild pid");

    assert_eq!(
        unsafe { libc::kill(grandchild, 0) },
        0,
        "grandchild should be alive before the exit-kill"
    );

    // Register the group leader the way `fetch_remote_silent` does, then run
    // the quit-time kill.
    register_killable_pid(child.id() as i32);
    kill_in_flight_git_ops();
    let _ = child.wait();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let rc = unsafe { libc::kill(grandchild, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "grandchild still alive 5s after the exit-kill"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A stalled mutating op must be killed as a process group once it exceeds its
/// timeout, the same way the fetch path kills a timed-out fetch. The command
/// forks a `sleep` grandchild, so a kill that took only the direct child would
/// leave that sleep (the ssh stand-in) running.
#[test]
fn capture_with_timeout_kills_the_group() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let pidfile = tempfile::NamedTempFile::new().expect("temp pid file");
    let pidfile_path = pidfile.path().to_str().unwrap().to_string();
    // Record the sh leader pid and the sleep grandchild pid, then block on the
    // sleep so the command cannot finish before the timeout.
    let script = format!(
        "echo $$ > '{pidfile_path}'; sleep 60 2>/dev/null & echo $! >> '{pidfile_path}'; wait",
    );
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let res = capture_with_timeout(&mut child, Duration::from_millis(1000));
    assert!(
        res.is_err(),
        "a sleeping command should time out, got {:?}",
        res.as_ref().map(|o| o.status.code())
    );

    // Both pids were written before `wait`, so read them after the timeout.
    let contents = std::fs::read_to_string(&pidfile_path).expect("read pid file");
    let mut lines = contents.lines();
    let leader: i32 = lines
        .next()
        .expect("leader pid")
        .parse()
        .expect("leader int");
    let grandchild: i32 = lines
        .next()
        .expect("grandchild pid")
        .parse()
        .expect("grandchild int");

    // The leader is reaped by `child.wait()`; the grandchild by init. Both must
    // be gone after the group kill.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    for pid in [leader, grandchild] {
        loop {
            let rc = unsafe { libc::kill(pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} still alive 5s after the timeout kill"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A git op that exits cleanly while a descendant still holds the pipe must
/// return Ok without killing that descendant. This guards the success-path drain:
/// it must be deadline-bounded (not a group kill), so a child that exits promptly
/// is not turned into an error and the ssh/hook stand-in survives.
#[test]
fn capture_with_timeout_success_does_not_kill_descendant() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // Write a line, background a sleep that keeps the pipe open, then exit 0.
    // The sleep is the ssh/hook stand-in; it must survive the success path.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("echo hi; sleep 60 2>/dev/null & echo $!; exit 0")
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let res = capture_with_timeout(&mut child, Duration::from_secs(5));
    assert!(
        res.is_ok(),
        "expected Ok from a prompt exit, got {:?}",
        res.as_ref().err().map(|e| e.kind())
    );
    let out = res.unwrap();
    assert_eq!(out.status.code(), Some(0));

    // stdout carries "hi\n<sleep pid>\n".
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some("hi"),
        "stdout should start with 'hi', got {stdout:?}"
    );
    let sleep_pid: i32 = lines
        .next()
        .expect("sleep pid")
        .parse()
        .expect("sleep pid int");

    // The sleep grandchild must still be alive — we must not have killed the group.
    assert_eq!(
        unsafe { libc::kill(sleep_pid, 0) },
        0,
        "sleep grandchild should still be alive after the success path",
    );

    // Clean up the lingering sleep.
    unsafe { libc::kill(sleep_pid, libc::SIGKILL) };
}

/// The public `run_git_op_capturing` must register the child at spawn so a
/// shutdown kill can take it (and its descendants) down, then unregister it. A
/// `!`-alias makes git fork `sh`, which backgrounds a `sleep` (the ssh stand-in)
/// and records its pid, so the test can prove the whole group died.
#[test]
fn run_git_op_capturing_registers_and_unregisters() {
    use std::time::Duration;
    let _lock = super::TEST_KILL_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _reset = ResetsShuttingDown::new();

    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();
    let init = std::process::Command::new("git")
        .arg("init")
        .arg(&path)
        .status();
    // Skip (don't panic) when git is unavailable, matching the repo's pattern.
    if !matches!(init, Ok(s) if s.success()) {
        eprintln!("skipping run_git_op_capturing test: 'git init' failed");
        return;
    }

    // The alias records the sleep pid in a file once the whole git -> sh ->
    // sleep tree is up; waiting on that (rather than on registration alone)
    // avoids killing the group before the stand-in has even been forked.
    let pidfile = tempfile::NamedTempFile::new().expect("temp pid file");
    let pidfile_path = pidfile.path().to_str().unwrap().to_string();
    let args = vec![
        "-c".to_string(),
        format!("alias.slow=!sleep 30 2>/dev/null & echo $! > '{pidfile_path}'; wait"),
        "slow".to_string(),
    ];
    let worker = std::thread::spawn(move || super::run_git_op_capturing(&path, &args));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let sleep_pid: i32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pidfile_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "git alias never reported its sleep grandchild"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // spawn() registers before it returns, so by now the child is in the set.
    assert!(
        !super::killable_pids().lock().unwrap().is_empty(),
        "run_git_op_capturing did not register its child"
    );

    let killed_at = std::time::Instant::now();
    kill_in_flight_git_ops();
    let out = worker
        .join()
        .expect("worker thread")
        .expect("a killed op still yields its captured Output");
    assert!(
        killed_at.elapsed() < Duration::from_secs(10),
        "wrapper did not return promptly after the shutdown kill"
    );
    assert!(
        !out.status.success(),
        "a SIGKILLed git op must not report success"
    );

    // The sleep grandchild must be gone too, otherwise the group kill missed
    // the ssh stand-in.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rc = unsafe { libc::kill(sleep_pid, 0) };
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sleep grandchild {sleep_pid} still alive after the shutdown kill"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        super::killable_pids().lock().unwrap().is_empty(),
        "killable registry not empty after run_git_op_capturing"
    );
}
