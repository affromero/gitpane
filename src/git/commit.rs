//! Commit changes in the checkout shown in the Changes panel.

use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

/// Hooks may run linters or tests, so allow as long as other mutating ops.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    Committed(String),
    NothingStaged,
}

/// Use the current index to decide whether a commit can proceed.
pub(crate) fn commit(path: &Path, message: &str, stage_all: bool) -> Result<CommitOutcome, String> {
    let staged = has_staged_changes(path)?;
    if stage_all {
        if staged {
            return Err(
                "The index changed. Submit again to commit only the staged changes.".into(),
            );
        }
        git(path, &["add", "--all"])?;
    } else if !staged {
        return Ok(CommitOutcome::NothingStaged);
    }
    // A message may start with `-` or outgrow an argument, so pass a file.
    let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    file.write_all(message.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|e| e.to_string())?;
    let message_path = file.path().to_string_lossy().into_owned();
    git(path, &["commit", "-F", &message_path])?;
    let summary = git(path, &["log", "-1", "--format=%h %s"])?;
    Ok(CommitOutcome::Committed(summary.trim().to_owned()))
}

fn has_staged_changes(path: &Path) -> Result<bool, String> {
    let output = run_git(
        path,
        &[
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--ignore-submodules=none",
            "--",
        ],
    )?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(failure_text(&output)),
    }
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git(path, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(failure_text(&output))
    }
}

fn run_git(path: &Path, args: &[&str]) -> Result<Output, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    crate::git::process::run_git_op_noninteractive(path, &args, COMMIT_TIMEOUT).map_err(|error| {
        format!(
            "git {}: {}",
            args[0],
            crate::git::describe_spawn_error(&error)
        )
    })
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
