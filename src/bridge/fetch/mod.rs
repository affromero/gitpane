use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{BridgeError, changes::RepositoryIdentity, scope::Scope};

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preview {
    repo: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fetch {
    repo: PathBuf,
    expected_repo_identity: String,
    expected_fetch_token: String,
}

#[derive(PartialEq, Eq)]
struct Selection {
    repo: PathBuf,
    identity: String,
    token: String,
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Repository or fetch configuration changed; refresh before fetching.",
    )
}

fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn symbolic_head(repo: &Path) -> Result<Option<String>, BridgeError> {
    let repository = git2::Repository::open(repo)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let head = repository
        .find_reference("HEAD")
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    match head
        .symbolic_target()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?
    {
        Some(target) => Ok(Some(target.to_owned())),
        None if head.target().is_some() => Ok(None),
        None => Err(BridgeError::new(
            "repository_error",
            "Cannot determine the repository HEAD",
        )),
    }
}

#[cfg(unix)]
fn effective_config(repo: &Path) -> Result<Vec<u8>, BridgeError> {
    let args = ["config", "--includes", "--null", "--list"].map(str::to_owned);
    let output =
        crate::git::process::run_git_op_noninteractive(repo, &args, Duration::from_secs(15))
            .map_err(|error| {
                BridgeError::new(
                    "git_error",
                    format!("Cannot inspect Git configuration: {error}"),
                )
            })?;
    if !output.status.success() {
        return Err(BridgeError::new(
            "git_error",
            "Cannot inspect Git configuration; check your Git configuration and try again.",
        ));
    }
    // The canonical helper bounds captured output at 64 KiB; reject the bound
    // rather than fingerprinting a potentially truncated configuration.
    if output.stdout.len() >= 64 * 1024 {
        return Err(BridgeError::new(
            "git_error",
            "Git configuration is too large to validate safely.",
        ));
    }
    Ok(output.stdout)
}

#[cfg(not(unix))]
fn effective_config(_repo: &Path) -> Result<Vec<u8>, BridgeError> {
    Err(BridgeError::new(
        "unsupported",
        "Fetch is not supported on this platform.",
    ))
}

fn selection(scope: &Scope, input: &Path) -> Result<Selection, BridgeError> {
    let repo = scope.repository(input)?;
    let identity = RepositoryIdentity::read(&repo)?;
    let head = symbolic_head(&repo)?;
    let config = effective_config(&repo)?;
    if scope.repository(input)? != repo
        || RepositoryIdentity::read(&repo)? != identity
        || symbolic_head(&repo)? != head
    {
        return Err(stale());
    }
    let mut hash = Sha256::new();
    match head {
        Some(name) => {
            hash.update([1]);
            hash.update((name.len() as u64).to_be_bytes());
            hash.update(name.as_bytes());
        }
        None => hash.update([0]),
    }
    hash.update((config.len() as u64).to_be_bytes());
    hash.update(config);
    Ok(Selection {
        repo,
        identity: identity.fingerprint()?,
        token: crate::hash::hex(hash.finalize().as_ref()),
    })
}

pub(super) fn preview(scope: &Scope, input: Preview) -> Result<Value, BridgeError> {
    let first = selection(scope, &input.repo)?;
    if selection(scope, &input.repo)? != first {
        return Err(stale());
    }
    Ok(json!({"repo_identity": first.identity, "fetch_token": first.token}))
}

#[cfg(unix)]
fn failure_hint(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    let contains = |patterns: &[&str]| patterns.iter().any(|pattern| text.contains(pattern));
    if contains(&[
        "could not resolve host",
        "name or service not known",
        "temporary failure in name resolution",
        "nodename nor servname provided",
    ]) {
        "Git could not resolve the remote host. Check the host name and DNS."
    } else if contains(&[
        "authentication failed",
        "permission denied (publickey)",
        "could not read username",
        "invalid username or password",
        "access denied",
    ]) {
        "Git authentication failed. Check the remote credentials and access."
    } else if contains(&[
        "repository not found",
        "does not appear to be a git repository",
        "no such file or directory",
    ]) {
        "Git could not find the remote repository. Check the remote and access."
    } else if contains(&[
        "rejected",
        "non-fast-forward",
        "cannot lock ref",
        "would clobber existing",
    ]) {
        "Git could not update one or more remote-tracking refs. Inspect the repository before retrying."
    } else if contains(&[
        "host key verification failed",
        "certificate verify failed",
        "ssl certificate problem",
    ]) {
        "Git could not verify the remote host or certificate. Check your SSH or TLS configuration."
    } else if contains(&[
        "could not connect",
        "connection refused",
        "network is unreachable",
        "connection timed out",
        "operation timed out",
    ]) {
        "Git could not connect to the remote. Check network access and retry."
    } else {
        "Check the remote and credentials, then refresh before retrying."
    }
}

#[cfg(unix)]
fn run(repo: &Path) -> Result<Value, BridgeError> {
    let args = [
        "-c",
        "submodule.recurse=false",
        "-c",
        "fetch.recurseSubmodules=false",
        "fetch",
        "--no-recurse-submodules",
    ]
    .map(str::to_owned);
    let output = crate::git::process::run_git_op_noninteractive(repo, &args, Duration::from_secs(30))
        .map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound { "git_error" } else { "outcome_unknown" };
            BridgeError::new(code, format!("Fetch did not complete: {error}. Remote-tracking refs may have changed; refresh before retrying."))
        })?;
    if !output.status.success() {
        return Err(BridgeError::new(
            "git_error",
            format!(
                "Fetch failed ({}). {}",
                output.status,
                failure_hint(&output.stderr),
            ),
        ));
    }
    Ok(json!({}))
}

#[cfg(not(unix))]
fn run(_repo: &Path) -> Result<Value, BridgeError> {
    Err(BridgeError::new(
        "unsupported",
        "Fetch is not supported on this platform.",
    ))
}

pub(super) fn execute(scope: &Scope, input: Fetch) -> Result<Value, BridgeError> {
    if !valid_token(&input.expected_repo_identity) || !valid_token(&input.expected_fetch_token) {
        return Err(BridgeError::new(
            "invalid_request",
            "Valid repository and fetch identity tokens are required.",
        ));
    }
    let first = selection(scope, &input.repo)?;
    if first.identity != input.expected_repo_identity || first.token != input.expected_fetch_token {
        return Err(stale());
    }
    let current = selection(scope, &input.repo)?;
    if current != first {
        return Err(stale());
    }
    run(&current.repo)
}
