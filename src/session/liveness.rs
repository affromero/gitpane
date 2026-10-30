//! Detects which repos/worktrees have a live tmux session by reading each
//! pane's session name and cwd. tmux-only: when gitpane is not under tmux (or
//! tmux is unavailable) the probe yields nothing and no markers are shown.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static WORKER_CANCELLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn worker_cancelled() -> bool {
    WORKER_CANCELLED.load(Ordering::SeqCst)
}

#[cfg(unix)]
pub(crate) fn install_worker_cancellation() -> std::io::Result<()> {
    extern "C" fn cancel(_signal: libc::c_int) {
        WORKER_CANCELLED.store(true, Ordering::SeqCst);
    }
    // The dedicated process owns its child lifecycle. An inherited ignored
    // SIGCHLD or SA_NOCLDWAIT would allow leader PID reuse during cleanup.
    let mut child_action: libc::sigaction = unsafe { std::mem::zeroed() };
    child_action.sa_sigaction = libc::SIG_DFL;
    unsafe { libc::sigemptyset(&mut child_action.sa_mask) };
    if unsafe { libc::sigaction(libc::SIGCHLD, &child_action, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = cancel as *const () as usize;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    if unsafe { libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn install_worker_cancellation() -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn cancellation_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "session worker cancelled")
}

#[cfg(all(test, unix))]
#[path = "liveness_worker_tests.rs"]
mod worker_tests;

/// Probe the selected host multiplexer without hiding process or parse errors.
pub(crate) fn checked_pane_sessions(kind: &str) -> color_eyre::Result<Vec<(String, PathBuf)>> {
    checked_pane_sessions_with(kind, std::process::Command::new(kind))
}

fn checked_pane_sessions_with(
    kind: &str,
    mut command: std::process::Command,
) -> color_eyre::Result<Vec<(String, PathBuf)>> {
    match kind {
        "tmux" => {
            command.args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_current_path}\t#{session_name}",
            ]);
        }
        "herdr" => {
            command.args(["pane", "list"]);
        }
        _ => return Err(color_eyre::eyre::eyre!("unknown session backend")),
    }
    let output = bounded_output(&mut command)?;
    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "{kind} session probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let panes = if kind == "tmux" {
        let parsed = parse_pane_sessions(text);
        if parsed.len() != text.lines().filter(|line| !line.is_empty()).count() {
            return Err(color_eyre::eyre::eyre!("invalid tmux session output"));
        }
        parsed
    } else {
        parse_herdr_panes(text).map_err(|message| color_eyre::eyre::eyre!(message))?
    };
    panes
        .into_iter()
        .map(|(name, cwd)| {
            let cwd = cwd
                .canonicalize()
                .map_err(|_| color_eyre::eyre::eyre!("Cannot resolve a session directory"))?;
            Ok((name, cwd))
        })
        .collect()
}

/// Run a session probe or explicit goto with a process-group deadline.
#[cfg(unix)]
pub(crate) fn bounded_output(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if worker_cancelled() {
        return Err(cancellation_error());
    }
    ensure_child_reaping_enabled()?;
    let mut child = crate::git::process::spawn_killable(command)?;
    let pid = child.id() as i32;
    let result = capture_session_output(&mut child, std::time::Duration::from_secs(15));
    crate::git::process::unregister_killable_pid(pid);
    result
}

#[cfg(unix)]
fn ensure_child_reaping_enabled() -> std::io::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "session probes require SIGCHLD child reaping; disable SIG_IGN and SA_NOCLDWAIT",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn capture_session_output(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::os::fd::AsRawFd;
    const MAX_OUTPUT: usize = 1024 * 1024;
    let deadline = std::time::Instant::now() + timeout;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut exited = false;
    let mut drain_deadline = None;
    loop {
        if worker_cancelled() {
            cleanup_session_child(child);
            return Err(cancellation_error());
        }
        let mut fds = [
            libc::pollfd {
                fd: stdout.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stderr.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 20) };
        let poll_error = (rc < 0).then(std::io::Error::last_os_error);
        if poll_error
            .as_ref()
            .is_some_and(|error| error.kind() != std::io::ErrorKind::Interrupted)
        {
            cleanup_session_child(child);
            return Err(poll_error.expect("poll error"));
        }
        let read_result = if fds[0].revents != 0 {
            read_session_pipe(&mut stdout, &mut out)
        } else {
            Ok(())
        }
        .and_then(|()| {
            if fds[1].revents != 0 {
                read_session_pipe(&mut stderr, &mut err)
            } else {
                Ok(())
            }
        });
        if let Err(error) = read_result {
            cleanup_session_child(child);
            return Err(error);
        }
        if out.len().saturating_add(err.len()) > MAX_OUTPUT {
            cleanup_session_child(child);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "session process output exceeds the size limit",
            ));
        }
        if !exited {
            match child_exited_without_reaping(child) {
                Ok(true) => {
                    exited = true;
                    drain_deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                }
                Ok(false) => {}
                Err(error) => {
                    cleanup_session_child(child);
                    return Err(error);
                }
            }
        }
        if exited {
            if stdout.is_none() && stderr.is_none() {
                return Ok(std::process::Output {
                    status: child.wait()?,
                    stdout: out,
                    stderr: err,
                });
            }
            if drain_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                cleanup_session_child(child);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "session process output did not close",
                ));
            }
        }
        if !exited && std::time::Instant::now() >= deadline {
            cleanup_session_child(child);
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "session process timed out",
            ));
        }
    }
}

#[cfg(unix)]
fn cleanup_session_child(child: &mut std::process::Child) {
    // A SIGCHLD disposition of SIG_IGN or an external reaper can already have
    // released the leader PID. Never signal its process group in that case.
    if child_exited_without_reaping(child).is_ok() {
        crate::git::process::kill_process_group(child);
        let _ = child.wait();
    }
}

#[cfg(unix)]
fn child_exited_without_reaping(child: &std::process::Child) -> std::io::Result<bool> {
    // Keeping the group leader unreaped prevents its PID being recycled before
    // cleanup of a descendant that still holds an output pipe.
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn read_session_pipe<R: std::io::Read>(
    pipe: &mut Option<R>,
    output: &mut Vec<u8>,
) -> std::io::Result<()> {
    let mut buffer = [0; 8192];
    if let Some(reader) = pipe {
        match reader.read(&mut buffer) {
            Ok(0) => *pipe = None,
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn bounded_output(
    _command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "session integration is not supported on this platform",
    ))
}

/// `(session_name, pane_cwd)` for every tmux pane across all sessions, via a
/// single `tmux list-panes -a` call. Empty when tmux is absent or errors.
pub(crate) fn tmux_pane_sessions() -> Vec<(String, PathBuf)> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_current_path}\t#{session_name}",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => parse_pane_sessions(&String::from_utf8_lossy(&o.stdout))
            .into_iter()
            // Canonicalize each pane cwd so it matches the canonicalized repo /
            // worktree paths even through symlinks (e.g. macOS /tmp ->
            // /private/tmp). Fall back to the raw path if it no longer exists.
            .map(|(s, p)| {
                let canon = p.canonicalize().unwrap_or(p);
                (s, canon)
            })
            .collect(),
        Ok(o) => {
            rate_limited_debug(|| {
                format!(
                    "tmux list-panes failed (status {:?}): {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            });
            Vec::new()
        }
        Err(e) => {
            rate_limited_debug(|| format!("tmux list-panes spawn failed: {e}"));
            Vec::new()
        }
    }
}

/// Log a debug line at most once per minute. The liveness probes run every
/// poll (5s by default), so a persistently failing probe would otherwise
/// spam the log; the rate limit keeps the failure diagnosable without noise.
fn rate_limited_debug(msg: impl FnOnce() -> String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 60
        && LAST
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::debug!("{}", msg());
    }
}

/// Parse `<pane_cwd>\t<session>` lines into `(session, path)` pairs. The path is
/// first so a tab in a session name lands in (and stays in) the session field
/// rather than corrupting the path. Lines without a tab, an empty session, or
/// an empty path are skipped.
fn parse_pane_sessions(output: &str) -> Vec<(String, PathBuf)> {
    output
        .lines()
        .filter_map(|line| {
            let (path, session) = line.split_once('\t')?;
            let path = path.trim();
            let session = session.trim();
            if session.is_empty() || path.is_empty() {
                return None;
            }
            Some((session.to_string(), PathBuf::from(path)))
        })
        .collect()
}

/// Probe herdr panes: `(tab_id, pane_cwd)` for every pane that reports a cwd.
/// The tab id is the handle herdr's attach (`herdr tab focus`) takes, the herdr
/// analog of a tmux session name. Prefers `foreground_cwd` (what a running
/// agent is actually working in) over the pane's label cwd. Empty when herdr
/// is absent or errors.
pub(crate) fn herdr_live_panes() -> Vec<(String, PathBuf)> {
    let output = std::process::Command::new("herdr")
        .args(["pane", "list"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            match parse_herdr_panes(&String::from_utf8_lossy(&o.stdout)) {
                Ok(panes) => panes
                    .into_iter()
                    .map(|(s, p)| {
                        let canon = p.canonicalize().unwrap_or(p);
                        (s, canon)
                    })
                    .collect(),
                Err(e) => {
                    rate_limited_debug(|| format!("herdr pane list parse failed: {e}"));
                    Vec::new()
                }
            }
        }
        Ok(o) => {
            rate_limited_debug(|| {
                format!(
                    "herdr pane list failed (status {:?}): {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            });
            Vec::new()
        }
        Err(e) => {
            rate_limited_debug(|| format!("herdr pane list spawn failed: {e}"));
            Vec::new()
        }
    }
}

/// Parse `herdr pane list` JSON (`.result.panes[]` of `{tab_id, cwd,
/// foreground_cwd}`) into `(tab_id, cwd)` pairs, dropping panes with no
/// resolvable cwd or tab id. `Err` carries the serde error so a shape
/// mismatch is diagnosable instead of collapsing to an empty set.
fn parse_herdr_panes(output: &str) -> Result<Vec<(String, PathBuf)>, String> {
    #[derive(serde::Deserialize)]
    struct Pane {
        #[serde(default)]
        tab_id: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        foreground_cwd: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Panes {
        panes: Vec<Pane>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        result: Panes,
    }
    let parsed: Envelope = serde_json::from_str(output).map_err(|e| e.to_string())?;
    Ok(parsed
        .result
        .panes
        .into_iter()
        .filter_map(|p| {
            // Prefer the foreground cwd (what a running agent is actually
            // working in); fall back to the pane's label cwd. Treat an empty
            // string as absent so `Some("")` falls through to `cwd` instead
            // of producing a bogus root-relative path.
            let cwd = p
                .foreground_cwd
                .filter(|c| !c.is_empty())
                .or_else(|| p.cwd.filter(|c| !c.is_empty()))?;
            let tab = p.tab_id.filter(|t| !t.is_empty())?;
            Some((tab, PathBuf::from(cwd)))
        })
        .collect())
}

/// Sorted, unique session names that have a pane cwd'd at or below `path` (the
/// repo/worktree is "live" in those sessions). Empty when none.
pub(crate) fn live_sessions(path: &Path, panes: &[(String, PathBuf)]) -> Vec<String> {
    let mut names: Vec<String> = panes
        .iter()
        .filter(|(_, p)| p.starts_with(path))
        .map(|(s, _)| s.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether `path` has any live session (a pane cwd'd at or below it). Used
/// for the bare `◉` row marker; the session names go in the context menu.
pub(crate) fn is_live(path: &Path, panes: &[(String, PathBuf)]) -> bool {
    panes.iter().any(|(_, p)| p.starts_with(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn checked_probe_reads_real_host_output_and_reports_host_failure() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("nested");
        std::fs::create_dir(&cwd).unwrap();
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "printf '%s\\t%s\\n' \"$1\" session", "probe"])
            .arg(&cwd);
        let panes = checked_pane_sessions_with("tmux", command).unwrap();
        assert_eq!(panes, vec![("session".into(), cwd.canonicalize().unwrap())]);
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf 'host failed' >&2; exit 1"]);
        let error = checked_pane_sessions_with("tmux", command).unwrap_err();
        assert!(error.to_string().contains("host failed"));
    }

    #[cfg(unix)]
    #[test]
    fn checked_probe_reports_unresolvable_session_directories() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf '%s' '{\"result\":{\"panes\":[{\"tab_id\":\"tab\",\"cwd\":\"/gitpane-nonexistent-session-test\"}]}}'"]);
        let error = checked_pane_sessions_with("herdr", command).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot resolve a session directory")
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_session_output_fails_before_excessive_output_accumulates() {
        let mut command = std::process::Command::new("sh");
        let chunk = "x".repeat(4096);
        command
            .arg("-c")
            .arg(format!("while :; do printf '%s' '{chunk}'; done"));
        let error = bounded_output(&mut command).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn incomplete_output_after_process_exit_kills_pipe_holding_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("descendant.pid");
        let escaped = pid_file.to_str().unwrap().replace('\'', "'\\''");
        let mut command = std::process::Command::new("sh");
        command.args(["-c", &format!("sleep 30 & echo $! > '{escaped}'")]);
        let error = bounded_output(&mut command).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with("Z "))
            }) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                unsafe { libc::kill(pid, libc::SIGKILL) };
                panic!("pipe-holding descendant {pid} survived the drain timeout");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn panes(ps: &[(&str, &str)]) -> Vec<(String, PathBuf)> {
        ps.iter()
            .map(|(s, p)| (s.to_string(), PathBuf::from(p)))
            .collect()
    }

    #[test]
    fn parse_skips_malformed_and_empty() {
        let out = "/code/app\tmain\nno-tab\n/code/x\t\n\twork\n/code/app/src\tside\n";
        assert_eq!(
            parse_pane_sessions(out),
            vec![
                ("main".to_string(), PathBuf::from("/code/app")),
                ("side".to_string(), PathBuf::from("/code/app/src")),
            ]
        );
    }

    #[test]
    fn parse_keeps_path_intact_when_session_name_has_tab() {
        // Path is the first field, so a tab in a session name can't corrupt it.
        assert_eq!(
            parse_pane_sessions("/code/app\tweird\tname\n"),
            vec![("weird\tname".to_string(), PathBuf::from("/code/app"))]
        );
    }

    #[test]
    fn live_sessions_are_prefix_matched_unique_and_sorted() {
        let p = panes(&[
            ("ternu", "/code/app/src"),
            ("fairtrail", "/code/app"),
            ("ternu", "/code/app"),         // duplicate session -> deduped
            ("other", "/code/app-sibling"), // sibling, not inside
        ]);
        assert_eq!(
            live_sessions(Path::new("/code/app"), &p),
            vec!["fairtrail".to_string(), "ternu".to_string()]
        );
    }

    #[test]
    fn live_sessions_empty_when_none_inside() {
        let p = panes(&[("x", "/elsewhere")]);
        assert!(live_sessions(Path::new("/code/app"), &p).is_empty());
    }

    #[test]
    fn is_live_matches_a_pane_inside_the_path() {
        let p = panes(&[("s", "/code/app/src")]);
        assert!(is_live(Path::new("/code/app"), &p));
        assert!(!is_live(Path::new("/code/app-sibling"), &p));
        assert!(!is_live(Path::new("/code/app"), &[]));
    }

    #[test]
    fn parse_herdr_panes_prefers_foreground_cwd_and_drops_null_tab() {
        let out = "{\"result\":{\"panes\":[
            {\"pane_id\":\"w1:p1\",\"tab_id\":\"w1:t1\",\"cwd\":\"/code/app\"},
            {\"pane_id\":\"w1:p2\",\"tab_id\":\"w1:t1\",\"cwd\":\"/code/app\",
             \"foreground_cwd\":\"/code/app/src\"},
            {\"pane_id\":\"w1:p3\",\"tab_id\":\"w1:t2\",\"cwd\":\"/elsewhere\"},
            {\"pane_id\":\"w1:p4\",\"tab_id\":null,\"cwd\":\"/code/app\"}
        ]}}";
        assert_eq!(
            parse_herdr_panes(out).unwrap(),
            vec![
                ("w1:t1".to_string(), PathBuf::from("/code/app")),
                ("w1:t1".to_string(), PathBuf::from("/code/app/src")),
                ("w1:t2".to_string(), PathBuf::from("/elsewhere")),
            ]
        );
    }

    #[test]
    fn parse_herdr_panes_skips_non_json_and_empty() {
        assert!(parse_herdr_panes("not json").is_err());
        // A missing `result` or `panes` (e.g. a future herdr renaming either
        // field) is an Err, so shape drift fails loudly instead of silently
        // showing "nothing live".
        assert!(parse_herdr_panes("{}").is_err());
        assert!(parse_herdr_panes("{\"result\":{}}").is_err());
        // A genuinely empty `panes: []` still parses to an empty set.
        assert!(
            parse_herdr_panes("{\"result\":{\"panes\":[]}}")
                .unwrap()
                .is_empty()
        );
        // A pane with a cwd but no tab id is dropped (nothing to focus).
        let out = "{\"result\":{\"panes\":[{\"pane_id\":\"w1:p1\",\"cwd\":\"/code\"}]}}";
        assert!(parse_herdr_panes(out).unwrap().is_empty());
    }

    #[test]
    fn parse_herdr_panes_falls_back_from_empty_foreground_cwd() {
        // An empty foreground_cwd is treated as absent: the label cwd wins.
        let out = "{\"result\":{\"panes\":[{\"pane_id\":\"w1:p1\",\"tab_id\":\"w1:t1\",
            \"cwd\":\"/code/app\",\"foreground_cwd\":\"\"}]}}";
        assert_eq!(
            parse_herdr_panes(out).unwrap(),
            vec![("w1:t1".to_string(), PathBuf::from("/code/app"))]
        );
    }
}
