//! `git commit` for the Changes panel's checkout, and the `[commit]
//! draft_command` that suggests a message for it.

use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

/// Hooks may run linters or tests, so allow as long as other mutating ops.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Commit what is staged in `path`, staging every change first when
/// `stage_all` is set. Returns the new commit as `<short hash> <subject>`, or
/// what Git and its hooks printed when a step fails.
pub(crate) fn commit(path: &Path, message: &str, stage_all: bool) -> Result<String, String> {
    if stage_all {
        git(path, &["add", "--all"])?;
    }
    // A message may start with `-` or outgrow an argument, so pass a file.
    let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    file.write_all(message.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|e| e.to_string())?;
    let message_path = file.path().to_string_lossy().into_owned();
    git(path, &["commit", "-F", &message_path])?;
    Ok(git(path, &["log", "-1", "--format=%h %s"])?
        .trim()
        .to_string())
}

/// Run `command` with `sh -c` in `path` and return what it printed: the
/// message the commit box fills in.
pub(crate) fn draft(path: &Path, command: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("sh");
    crate::git::process::clear_inherited_git_env(&mut cmd);
    let output = cmd
        .arg("-c")
        .arg(command)
        .current_dir(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("draft command failed: {e}"))?;
    if !output.status.success() {
        return Err(failure_text(&output));
    }
    let message = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if message.is_empty() {
        return Err("draft command printed nothing".to_string());
    }
    Ok(message)
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let output = crate::git::process::run_git_op_noninteractive(path, &args, COMMIT_TIMEOUT)
        .map_err(|e| format!("git {}: {}", args[0], crate::git::describe_spawn_error(&e)))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(failure_text(&output))
    }
}

/// Git prints `nothing to commit` on stdout and hooks use either stream, so a
/// failure shows both.
fn failure_text(output: &Output) -> String {
    let text: Vec<String> = [&output.stdout, &output.stderr]
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    if text.is_empty() {
        format!("exited with {}", output.status)
    } else {
        text.join("\n")
    }
}

#[cfg(test)]
mod tests;
