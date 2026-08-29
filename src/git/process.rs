//! Process-group kill and timed command capture for git operations.
//!
//! A git operation that can touch an SSH remote spawns an `ssh` child. When the
//! app exits (or is force-quit with a second `q`) with such an operation in
//! flight, or when it exceeds a timeout, we take the whole process group down
//! (git + its `ssh` child) rather than leaving the remote connection open. This
//! module owns that logic: a process-global killable registry, a deadline-
//! bounded capture, and the group-kill primitives.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Seconds a user-initiated mutating git op (pull/push/submodule, and the file
/// and worktree ops) may run before it is killed as a process group. Long
/// enough to let a large transfer finish, but bounded so a stalled connection
/// cannot hang the app forever (a normal quit waits for mutating ops to
/// complete).
const MUTATING_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a success-path [`capture_with_timeout`] may keep draining a child's
/// pipes after that child already exited, before giving up. A `git` command
/// that exits cleanly can leave a descendant (e.g. a backgrounded hook) holding
/// a pipe write-end open; we poll with this short grace rather than killing that
/// descendant, and rather than blocking for minutes.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Pids of in-flight git processes that should be taken down as a group when the
/// app exits. This covers the status poll's `git fetch` and the mutating
/// operations (push/pull/submodule) that can spawn an `ssh` child over a network
/// fetch; killing only the git process would orphan that ssh and keep the
/// server-side connection alive until the remote gives up.
///
/// Each process is spawned in its own process group (pgid == pid on unix), so an
/// exit-time kill of these pids takes the whole tree down. The lock is held
/// across `spawn`+registration. fork+exec of git is a few milliseconds, so
/// concurrent repo polls serialize their spawns for that window — harmless at
/// this scale — and it is what closes the spawn/kill race below. The lock is
/// never held while a process waits.
static KILLABLE_GIT_PIDS: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();

/// Set once the process is exiting, so a not-yet-started git process bails out
/// instead of registering after [`kill_in_flight_git_ops`] has snapshotted.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn killable_pids() -> &'static Mutex<HashSet<i32>> {
    KILLABLE_GIT_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Spawn `cmd` in its own process group (unix) and register its pid for the
/// shutdown kill. The lock is held across spawn+register so a
/// [`kill_in_flight_git_ops`] racing us either sees this pid or makes us bail.
/// Returns an `Interrupted` error once the process is shutting down.
pub(crate) fn spawn_killable(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = {
        let mut pids = killable_pids().lock().unwrap();
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "gitpane is shutting down",
            ));
        }
        let child = cmd.spawn()?;
        pids.insert(child.id() as i32);
        child
    };
    Ok(child)
}

pub(crate) fn unregister_killable_pid(pid: i32) {
    killable_pids().lock().unwrap().remove(&pid);
}

/// Kill every in-flight killable git process group, so quitting does not leave an
/// orphaned `ssh` (git-upload-pack / git-receive-pack) holding the remote
/// connection open. Sets `SHUTTING_DOWN` first so a process that has not spawned
/// yet bails out (see [`spawn_killable`]).
///
/// A process group that exits between the snapshot and the `killpg` and has its
/// pgid recycled could signal an unrelated group; the window is a few
/// microseconds and the pre-existing single-child kill has the same race, so it
/// is accepted.
///
/// Non-unix: the processes are not in their own process group (there is no pid
/// invariant to `killpg`), and killing by pid alone would miss ssh anyway, so
/// this is a no-op and the processes are left to finish on their own.
pub(crate) fn kill_in_flight_git_ops() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let pids: Vec<i32> = {
        let mut pids = killable_pids().lock().unwrap();
        let snapshot: Vec<i32> = pids.iter().copied().collect();
        pids.clear();
        snapshot
    };
    #[cfg(unix)]
    for pid in pids {
        // SAFETY: processes are spawned with process_group(0), so the pgid
        // equals the child pid. ESRCH (group already gone) is harmless.
        unsafe { libc::killpg(pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pids;
}

/// Run `git -C <path> <args>` capturing stdout/stderr, in a killable process
/// group so a force-quit can take it (and its `ssh` child) down, and a stalled
/// connection is bounded by [`MUTATING_OP_TIMEOUT`] rather than hanging the app.
/// Used by the mutating operations (push/pull/submodule/worktree/file).
#[cfg(unix)]
pub(crate) fn run_git_op_capturing(
    path: &Path,
    args: &[String],
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_killable(&mut cmd)?;
    let pid = child.id() as i32;
    let result = capture_with_timeout(&mut child, MUTATING_OP_TIMEOUT);
    unregister_killable_pid(pid);
    result
}

/// Non-unix has no process group to kill and no portable deadline-bounded pipe
/// drain, so fall back to the plain `Command::output()`, which drains both pipes
/// concurrently (the original behavior) and avoids a pipe-fill regression where
/// a large-output `git` op would block and be reported as a timeout.
#[cfg(not(unix))]
pub(crate) fn run_git_op_capturing(
    path: &Path,
    args: &[String],
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.output()
}

/// Drain a spawned `child`'s stdout/stderr while waiting at most `timeout`,
/// killing the whole process group (git + its ssh child) if it does not exit in
/// time. Split out from [`run_git_op_capturing`] so tests can drive a short
/// timeout without going through the process-global killable registry.
///
/// `child` must be in its own process group (spawned by [`spawn_killable`] with
/// `process_group(0)`, or via `.process_group(0)` in tests) so a timeout kill
/// targets the git child and its descendants rather than the caller's group.
#[cfg(unix)]
pub(super) fn capture_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let deadline = std::time::Instant::now() + timeout;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut chunk = [0u8; 8192];

    // Drain both pipes while waiting for the child, so a chatty git command can't
    // fill a pipe and block itself (which would otherwise turn a long op into a
    // false timeout).
    let status = loop {
        read_ready(stdout.as_mut(), &mut out, &mut chunk);
        read_ready(stderr.as_mut(), &mut err, &mut chunk);
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {}
            Err(e) => {
                // Spawn/wait error — kill the whole group so a still-running
                // child is not left untracked, then reap.
                kill_process_group(child);
                let _ = child.wait();
                return Err(e);
            }
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    match status {
        Some(st) => {
            // The child exited. Drain the tail with a short grace so a descendant
            // still holding a pipe (e.g. a backgrounded hook) can't block us,
            // without killing that (possibly legitimate) descendant — a
            // success-path group kill would. Both pipes are polled together so
            // stderr (which carries error messages) can't be starved.
            let (o2, e2) = drain_pipes(stdout.as_mut(), stderr.as_mut(), DRAIN_GRACE);
            out.extend(o2);
            err.extend(e2);
            Ok(std::process::Output {
                status: st,
                stdout: out,
                stderr: err,
            })
        }
        None => {
            // Timed out — kill the whole process group (git + its ssh child) and
            // reap it; the group kill closes the pipe write-ends.
            kill_process_group(child);
            let _ = child.wait();
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out after {}s", timeout.as_secs()),
            ))
        }
    }
}

/// Read whatever is currently available on a pipe without blocking (unix: poll
/// with a 0 timeout, then drain). Used to drain a child's output while it is
/// still running, so a chatty command can't fill the pipe and block. Bounded to
/// a fixed number of reads per pass so a child streaming stdout without pause
/// can't starve the stderr drain and the `try_wait`/deadline checks.
#[cfg(unix)]
fn read_ready<R>(r: Option<&mut R>, buf: &mut Vec<u8>, chunk: &mut [u8])
where
    R: std::io::Read + std::os::fd::AsRawFd,
{
    let Some(r) = r else { return };
    for _ in 0..64 {
        let mut pfd = libc::pollfd {
            fd: r.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
        if rc <= 0 {
            break;
        }
        match r.read(chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Drain a child's stdout and stderr pipes (polling both together) until `grace`
/// elapses or both hit EOF. A git child that exits cleanly can leave a descendant
/// holding a pipe write-end; a plain `read_to_end` would block forever, and
/// draining one then the other can starve the second (losing stderr's error
/// message). Polling both in one loop bounds the wait and avoids starving either.
/// Returns whatever was read; the pipes are not killed.
#[cfg(unix)]
fn drain_pipes(
    mut stdout: Option<&mut std::process::ChildStdout>,
    mut stderr: Option<&mut std::process::ChildStderr>,
    grace: std::time::Duration,
) -> (Vec<u8>, Vec<u8>) {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now() + grace;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut done = [false, false];
    loop {
        if std::time::Instant::now() >= deadline || (done[0] && done[1]) {
            break;
        }
        let mut fds = [
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
        ];
        if !done[0]
            && let Some(o) = stdout.as_deref()
        {
            fds[0].fd = o.as_raw_fd();
            fds[0].events = libc::POLLIN;
        }
        if !done[1]
            && let Some(e) = stderr.as_deref()
        {
            fds[1].fd = e.as_raw_fd();
            fds[1].events = libc::POLLIN;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if rc < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if rc == 0 {
            break; // grace elapsed, nothing readable
        }
        if fds[0].revents != 0 && !done[0] {
            match stdout.as_deref_mut().map(|o| o.read(&mut chunk)) {
                Some(Ok(0)) => done[0] = true,
                Some(Ok(n)) => out.extend_from_slice(&chunk[..n]),
                Some(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Some(Err(_)) => done[0] = true,
                None => done[0] = true,
            }
        }
        if fds[1].revents != 0 && !done[1] {
            match stderr.as_deref_mut().map(|e| e.read(&mut chunk)) {
                Some(Ok(0)) => done[1] = true,
                Some(Ok(n)) => err.extend_from_slice(&chunk[..n]),
                Some(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Some(Err(_)) => done[1] = true,
                None => done[1] = true,
            }
        }
    }
    (out, err)
}

/// Kill the child and everything in its process group.
///
/// On unix the child was spawned by [`spawn_killable`] with `process_group(0)`,
/// so its pgid equals its pid and this takes `git fetch` and its `ssh` child down
/// together. Non-unix has no reliable process-group kill for the ssh grandchild,
/// so it falls back to killing only the direct child (previous behavior); the
/// orphan-ssh fix is therefore unix-scoped.
#[cfg(unix)]
pub(crate) fn kill_process_group(child: &mut std::process::Child) {
    // SAFETY: child.id() is the pgid because spawn_killable spawned it with
    // process_group(0). A race where the group already exited is harmless
    // (ESRCH). Any other failure (rare, e.g. EPERM) falls back to killing the
    // direct child so a still-running git is not left unkilled; a surviving ssh
    // grandchild in that exceptional case is accepted.
    let pgid = child.id() as i32;
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } != 0
        && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
pub(crate) fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// Test hook: register a pid so [`kill_in_flight_git_ops`] can be exercised
/// without running a real git command.
#[cfg(test)]
pub(super) fn register_killable_pid(pid: i32) {
    killable_pids().lock().unwrap().insert(pid);
}

/// Test RAII guard that resets the one-way `SHUTTING_DOWN` flag on drop, so a
/// test that drives the exit-kill cannot leave the process-global flag set (even
/// if the test panics) and poison a later test that spawns a killable git
/// process.
#[cfg(test)]
pub(super) struct ResetsShuttingDown;

#[cfg(test)]
impl ResetsShuttingDown {
    pub(super) fn new() -> Self {
        Self
    }
}

#[cfg(test)]
impl Drop for ResetsShuttingDown {
    fn drop(&mut self) {
        SHUTTING_DOWN.store(false, Ordering::SeqCst);
    }
}

/// Serializes the tests that touch the process-global `KILLABLE_GIT_PIDS` /
/// `SHUTTING_DOWN` state, so a test that sets `SHUTTING_DOWN` cannot race a
/// test that spawns a killable git process.
#[cfg(test)]
static TEST_KILL_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests;
