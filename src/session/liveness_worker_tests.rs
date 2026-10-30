use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODE: &str = "GITPANE_LIVENESS_WORKER_TEST_MODE";
const DIRECTORY: &str = "GITPANE_LIVENESS_WORKER_TEST_DIRECTORY";

#[test]
fn worker_subprocess_helper() {
    let Some(mode) = std::env::var_os(MODE) else {
        return;
    };
    let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
    if mode == "ignored_sigchld" || mode == "no_child_wait" {
        if mode == "ignored_sigchld" {
            unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        } else {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = libc::SIG_DFL;
            action.sa_flags = libc::SA_NOCLDWAIT;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
                0
            );
        }
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let error = bounded_output(&mut command).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("SIGCHLD"));
        return;
    }
    if mode == "ignored_active_probe" {
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    }
    install_worker_cancellation().unwrap();
    ensure_child_reaping_enabled().unwrap();
    if mode == "before_spawn" {
        std::fs::write(directory.join("ready"), "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker_cancelled() {
            assert!(Instant::now() < deadline, "worker was not cancelled");
            std::thread::sleep(Duration::from_millis(10));
        }
        let marker = directory.join("spawned");
        let escaped = marker.to_str().unwrap().replace('\'', "'\\''");
        let mut command = Command::new("sh");
        command.args(["-c", &format!("printf spawned > '{escaped}'")]);
        assert_eq!(
            bounded_output(&mut command).unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(!marker.exists());
        return;
    }
    assert!(mode == "active_probe" || mode == "ignored_active_probe");
    let pid_file = directory.join("host.pid");
    let escaped = pid_file.to_str().unwrap().replace('\'', "'\\''");
    let ready = directory.join("host.ready");
    let escaped_ready = ready.to_str().unwrap().replace('\'', "'\\''");
    let mut command = Command::new("sh");
    command.args([
        "-c",
        &format!("printf '%s\\n' $$ > '{escaped}'; : > '{escaped_ready}'; exec sleep 30"),
    ]);
    match bounded_output(&mut command) {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::Interrupted),
        Ok(output) => assert!(mode == "ignored_active_probe" && !output.status.success()),
    }
}

fn helper(mode: &str, dir: &Path, ignore_sigchld: bool) -> Child {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "session::liveness::worker_tests::worker_subprocess_helper",
            "--nocapture",
        ])
        .env(MODE, mode)
        .env(DIRECTORY, dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if ignore_sigchld {
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGCHLD, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    command.spawn().unwrap()
}

fn wait_for_file(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("worker exited before test setup: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("worker setup timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_success(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            if !status.success() {
                use std::io::Read;
                let mut errors = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut errors)
                    .unwrap();
                panic!("worker failed: {status}: {errors}");
            }
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("worker failed to exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shared_probe_rejects_unsafe_sigchld_dispositions() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = helper("ignored_sigchld", temp.path(), true);
    wait_success(&mut child);
    let mut child = helper("no_child_wait", temp.path(), false);
    wait_success(&mut child);
}

#[test]
fn worker_resets_ignored_sigchld_before_cleanup_during_probe_exit() {
    use std::os::unix::process::CommandExt;
    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut sentinel = Guard(
        Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let temp = tempfile::tempdir().unwrap();
    let mut child = helper("ignored_active_probe", temp.path(), true);
    let pid_file = temp.path().join("host.pid");
    wait_for_file(&temp.path().join("host.ready"), &mut child);
    let host: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    // Race probe exit with worker cleanup. The reset disposition keeps the
    // leader's PID reserved until the worker has finished the group check.
    unsafe { libc::kill(host, libc::SIGKILL) };
    wait_success(&mut child);
    assert!(
        sentinel.0.try_wait().unwrap().is_none(),
        "unrelated group was signaled"
    );
}

#[test]
fn cleanup_does_not_signal_a_group_after_the_leader_was_reaped() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("descendant.pid");
    let escaped = pid_file.to_str().unwrap().replace('\'', "'\\''");
    let mut command = Command::new("sh");
    command.args(["-c", &format!("sleep 30 & echo $! > '{escaped}'")]);
    let mut child = crate::git::process::spawn_killable(&mut command).unwrap();
    let leader = child.id() as i32;
    assert!(child.wait().unwrap().success());
    crate::git::process::unregister_killable_pid(leader);
    let descendant: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::getpgid(descendant) }, leader);
    assert_eq!(
        child_exited_without_reaping(&child)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ECHILD)
    );
    cleanup_session_child(&mut child);
    let survived = unsafe { libc::kill(descendant, 0) } == 0;
    // A live descendant still in the group anchors its ID while we clean up.
    if survived && unsafe { libc::getpgid(descendant) } == leader {
        unsafe { libc::killpg(leader, libc::SIGKILL) };
    }
    assert!(
        survived,
        "cleanup killed a process group it no longer owned"
    );
}

#[test]
fn cancellation_before_probe_prevents_host_spawn() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = helper("before_spawn", temp.path(), false);
    wait_for_file(&temp.path().join("ready"), &mut child);
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    wait_success(&mut child);
    assert!(!temp.path().join("spawned").exists());
}

#[test]
fn cancellation_kills_and_reaps_active_probe() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = helper("active_probe", temp.path(), false);
    let pid_file = temp.path().join("host.pid");
    wait_for_file(&temp.path().join("host.ready"), &mut child);
    let host: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    wait_success(&mut child);
    assert_eq!(unsafe { libc::kill(host, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
