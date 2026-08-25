//! Tests for the process-group kill used by the fetch timeout path.
//!
//! `git fetch` spawns an `ssh` child for ssh remotes; killing only the git
//! process leaves that ssh orphaned with its server-side connection alive.
//! The fix spawns the fetch in its own process group and kills the group, so
//! this test asserts that a process-group kill takes grandchildren down too.
#![cfg(unix)]

use super::worktree::kill_process_group;

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
