use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{BridgeError, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, changes, scope::Scope};

mod metadata;
mod runner;
#[cfg(windows)]
mod windows;

use runner::git;

const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u64,
    workspace_roots: Vec<PathBuf>,
    workspace_root_identities: Vec<String>,
    workspace_root_sources: Vec<PathBuf>,
    repo: PathBuf,
    expected_repo_identity: String,
    git_executable: PathBuf,
    operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Content {
        #[serde(rename = "ref")]
        reference: String,
        path: String,
    },
    VerifiedContent {
        #[serde(rename = "ref")]
        reference: String,
        path: String,
    },
    Metadata {
        items: Vec<Item>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Item {
    #[serde(rename = "ref")]
    reference: String,
    path: String,
}

fn invalid(message: impl Into<String>) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn failure(message: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", message.to_string())
}

fn validate_item(reference: &str, path: &str, content: bool) -> Result<(), BridgeError> {
    let valid_ref = matches!(reference, ":0" | ":1" | ":2" | ":3")
        || (!content && reference == "HEAD")
        || changes::blob::commit_oid(reference);
    if !valid_ref || !changes::blob::valid_path(path) {
        return Err(invalid("Invalid Git content reference or path."));
    }
    Ok(())
}

fn validate(request: &Request) -> Result<(), BridgeError> {
    if request.version != 1 || !request.git_executable.is_absolute() {
        return Err(invalid("Invalid worker version or Git executable."));
    }
    if request.expected_repo_identity.len() != 64
        || !request
            .expected_repo_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("Invalid repository identity."));
    }
    match &request.operation {
        Operation::Content { reference, path } => validate_item(reference, path, true)?,
        Operation::VerifiedContent { reference, path } => validate_item(reference, path, false)?,
        Operation::Metadata { items } => {
            if items.is_empty() || items.len() > 128 {
                return Err(invalid("Metadata requires between 1 and 128 items."));
            }
            for item in items {
                validate_item(&item.reference, &item.path, false)?;
            }
        }
    }
    check_scope(request)
}

fn check_scope(request: &Request) -> Result<(), BridgeError> {
    let scope = Scope::new_with_identities(
        request.workspace_roots.clone(),
        Some(request.workspace_root_identities.clone()),
        Some(request.workspace_root_sources.clone()),
    )?;
    let repo = scope.repository(&request.repo)?;
    if changes::RepositoryIdentity::read(&repo)?.fingerprint()? != request.expected_repo_identity {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed; refresh and try again.",
        ));
    }
    Ok(())
}

fn read_line(limit: u64) -> Result<Vec<u8>, BridgeError> {
    let mut line = Vec::new();
    let stdin = io::stdin();
    let mut input = stdin.lock().take(limit + 1);
    input.read_until(b'\n', &mut line).map_err(failure)?;
    if line.len() > limit as usize || !line.ends_with(b"\n") {
        return Err(invalid(
            "Worker request is incomplete or exceeds the size limit.",
        ));
    }
    Ok(line)
}

fn decode(bytes: &[u8]) -> Result<Request, BridgeError> {
    serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("Invalid worker request: {error}")))
}

fn exec() -> Result<(), BridgeError> {
    let line = read_line(MAX_REQUEST_BYTES + 3)?;
    let payload = line
        .strip_prefix(b"GO ")
        .ok_or_else(|| invalid("Missing worker gate."))?;
    let request = decode(payload)?;
    validate(&request)?;
    match &request.operation {
        Operation::Content { reference, path } => runner::content(&request, reference, path)?,
        Operation::VerifiedContent { reference, path } => {
            metadata::verified_content(&request, reference, path)?
        }
        Operation::Metadata { items } => {
            let result = metadata::metadata(&request, items)?;
            let bytes = serde_json::to_vec(&result).map_err(failure)?;
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(failure("Metadata response exceeds the size limit."));
            }
            io::stdout().write_all(&bytes).map_err(failure)?;
        }
    }
    check_scope(&request)
}

#[cfg(unix)]
pub(super) fn guard() -> ! {
    if unsafe { libc::getpgrp() != libc::getpid() } {
        std::process::exit(1);
    }
    let mut input = io::stdin();
    let mut buffer = [0u8; 256];
    while matches!(input.read(&mut buffer), Ok(count) if count > 0) {}
    unsafe {
        libc::kill(-libc::getpgrp(), libc::SIGKILL);
    }
    std::process::exit(1);
}

#[cfg(unix)]
fn cleanup_guardian(guardian: &mut std::process::Child) {
    // The unreaped guardian pins its process-group ID until this signal.
    unsafe {
        libc::kill(-(guardian.id() as i32), libc::SIGKILL);
    }
    drop(guardian.stdin.take());
    let _ = guardian.wait();
}

#[cfg(not(unix))]
pub(super) fn guard() -> ! {
    std::process::exit(1);
}

fn supervise() -> Result<(), BridgeError> {
    let started = Instant::now();
    let request = decode(&read_line(MAX_REQUEST_BYTES)?)?;
    validate(&request)?;
    #[cfg(unix)]
    if unsafe { libc::getpgrp() != libc::getpid() } {
        return Err(invalid("Worker requires its own process group."));
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = io::stdin();
        let mut byte = [0u8; 1];
        let _ = input.read(&mut byte);
        let _ = tx.send(());
    });
    if rx.try_recv().is_ok() {
        return Err(failure("Worker input closed."));
    }

    #[cfg(windows)]
    let job = windows::Job::new().map_err(failure)?;
    #[cfg(unix)]
    let mut guardian = {
        use std::os::unix::process::CommandExt;
        Command::new(std::env::current_exe().map_err(failure)?)
            .args(["serve", "--content-guard"])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(failure)?
    };
    let mut command = Command::new(std::env::current_exe().map_err(failure)?);
    command
        .args(["serve", "--content-exec"])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(guardian.id() as i32);
    }
    let mut child = command.spawn().map_err(failure)?;
    #[cfg(windows)]
    if let Err(error) = job.attach(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failure(format!("Cannot contain Git worker: {error}")));
    }
    let mut gate = child
        .stdin
        .take()
        .ok_or_else(|| failure("Worker gate unavailable."))?;
    let mut payload = b"GO ".to_vec();
    payload.extend(serde_json::to_vec(&request).map_err(failure)?);
    payload.push(b'\n');
    std::thread::spawn(move || {
        let _ = gate.write_all(&payload);
    });
    loop {
        if let Some(status) = child.try_wait().map_err(failure)? {
            #[cfg(windows)]
            drop(job);
            #[cfg(unix)]
            cleanup_guardian(&mut guardian);
            if status.success() {
                return Ok(());
            }
            std::process::exit(1);
        }
        if rx.try_recv().is_ok() || started.elapsed() >= DEADLINE {
            #[cfg(windows)]
            drop(job);
            #[cfg(unix)]
            cleanup_guardian(&mut guardian);
            let _ = child.kill();
            let _ = child.wait();
            return Err(BridgeError::new(
                "worker_cancelled",
                "Git content worker was cancelled or timed out.",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn main(exec_mode: bool) -> ! {
    let result = if exec_mode { exec() } else { supervise() };
    if let Err(error) = result {
        let message = error.message.chars().take(2048).collect::<String>();
        let _ = writeln!(
            io::stderr(),
            "{}",
            serde_json::json!({"code":error.code,"message":message})
        );
        std::process::exit(1);
    }
    std::process::exit(0);
}
