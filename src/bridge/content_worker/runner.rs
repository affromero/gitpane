use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use super::{BridgeError, Request, failure};

const STDERR_LIMIT: usize = 64 * 1024;

pub(super) struct GitOutput {
    pub(super) success: bool,
    pub(super) code: Option<i32>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) fn command(request: &Request) -> Command {
    let mut command = Command::new(&request.git_executable);
    crate::git::process::clear_inherited_git_env(&mut command);
    let mut git_dir = OsString::from("--git-dir=");
    git_dir.push(request.repo.join(".git"));
    let mut work_tree = OsString::from("--work-tree=");
    work_tree.push(&request.repo);
    command
        .current_dir(&request.repo)
        .arg(git_dir)
        .arg(work_tree);
    command
}

fn drain(mut input: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut result = Vec::new();
    let mut overflow = false;
    let mut buffer = [0u8; 8192];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let take = count.min(limit.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..take]);
        overflow |= take != count;
    }
    Ok((result, overflow))
}

pub(super) fn git(
    request: &Request,
    args: &[OsString],
    input: Option<&[u8]>,
    max_output: usize,
    remove_config: bool,
) -> Result<GitOutput, BridgeError> {
    run(request, args, input, max_output, remove_config, false)
}

pub(super) fn git_probe(
    request: &Request,
    args: &[OsString],
    input: Option<&[u8]>,
    max_output: usize,
) -> Result<GitOutput, BridgeError> {
    run(request, args, input, max_output, false, true)
}

fn run(
    request: &Request,
    args: &[OsString],
    input: Option<&[u8]>,
    max_output: usize,
    remove_config: bool,
    c_locale: bool,
) -> Result<GitOutput, BridgeError> {
    let mut command = command(request);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if remove_config {
        command.env_remove("GIT_CONFIG");
    }
    if c_locale {
        command.env("LC_ALL", "C").env("LANGUAGE", "C");
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn().map_err(failure)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure("Git stdout unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure("Git stderr unavailable."))?;
    let stdin = child.stdin.take();
    let (status, out, err) = std::thread::scope(|scope| {
        let out = scope.spawn(move || drain(stdout, max_output));
        let err = scope.spawn(move || drain(stderr, STDERR_LIMIT));
        let writer = scope.spawn(move || -> std::io::Result<()> {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(input)?;
            }
            Ok(())
        });
        let status = child.wait();
        let written = writer
            .join()
            .map_err(|_| failure("Git input thread failed."))?;
        let out = out
            .join()
            .map_err(|_| failure("Git output thread failed."))?
            .map_err(failure)?;
        let err = err
            .join()
            .map_err(|_| failure("Git error thread failed."))?
            .map_err(failure)?;
        written.map_err(failure)?;
        Ok::<_, BridgeError>((status.map_err(failure)?, out, err))
    })?;
    if out.1 || err.1 {
        return Err(failure("Git output exceeds the size limit."));
    }
    Ok(GitOutput {
        success: status.success(),
        code: status.code(),
        stdout: out.0,
        stderr: err.0,
    })
}

pub(super) fn content(request: &Request, reference: &str, path: &str) -> Result<(), BridgeError> {
    run_content(request, reference, path, false)
}

pub(super) fn content_drained(
    request: &Request,
    reference: &str,
    path: &str,
) -> Result<(), BridgeError> {
    run_content(request, reference, path, true)
}

fn run_content(
    request: &Request,
    reference: &str,
    path: &str,
    drain_output: bool,
) -> Result<(), BridgeError> {
    let mut argument = OsString::from(reference);
    argument.push(OsStr::new(":"));
    argument.push(OsStr::new(path));
    let mut child = command(request)
        .args([OsStr::new("show"), OsStr::new("--textconv"), &argument])
        .stdin(Stdio::null())
        .stdout(if drain_output {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::piped())
        .spawn()
        .map_err(failure)?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure("Git stderr unavailable."))?;
    let reader = std::thread::spawn(move || drain(stderr, STDERR_LIMIT));
    if drain_output {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("Git stdout unavailable."))?;
        if let Err(error) = std::io::copy(&mut stdout, &mut std::io::stdout().lock()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure(error));
        }
    }
    let status = child.wait().map_err(failure)?;
    let (stderr, overflow) = reader
        .join()
        .map_err(|_| failure("Git error thread failed."))?
        .map_err(failure)?;
    if !status.success() || overflow {
        let message = String::from_utf8_lossy(&stderr);
        return Err(failure(format!(
            "Git content read failed: {}",
            message.trim()
        )));
    }
    Ok(())
}
