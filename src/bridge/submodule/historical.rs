use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::bridge::{BridgeError, changes::RepositoryIdentity, scope::Scope};

use super::valid_submodule_path;

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct ContentRequest {
    repo: PathBuf,
    expected_repo_identity: String,
    commit_oid: String,
    path: PathBuf,
    expected_oid: String,
    expected_mode: i32,
}

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Historical submodule content changed; refresh and try again.",
    )
}

fn full_oid(value: &str) -> Result<git2::Oid, BridgeError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("A full Git object ID is required."));
    }
    git2::Oid::from_str(value).map_err(|_| invalid("Invalid Git object ID."))
}

pub(in crate::bridge) fn content(
    scope: &Scope,
    request: ContentRequest,
) -> Result<Value, BridgeError> {
    if request.expected_repo_identity.len() != 64
        || !request
            .expected_repo_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_submodule_path(&request.path)
        || !matches!(
            request.expected_mode,
            0o100644 | 0o100664 | 0o100755 | 0o120000
        )
    {
        return Err(invalid("Invalid historical submodule content request."));
    }
    let commit_oid = full_oid(&request.commit_oid)?;
    let expected_oid = full_oid(&request.expected_oid)?;
    let repo_path = scope.repository(&request.repo)?;
    scope.validate_submodules(&repo_path)?;
    let identity = RepositoryIdentity::read(&repo_path)?;
    if identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let commit = repo.find_commit(commit_oid).map_err(|_| stale())?;
    let tree = commit
        .tree()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let entry = tree.get_path(&request.path).map_err(|_| stale())?;
    if entry.id() != expected_oid || entry.filemode() != request.expected_mode {
        return Err(stale());
    }
    let error = |error: git2::Error| BridgeError::new("repository_error", error.to_string());
    let (size, kind) = repo
        .odb()
        .map_err(error)?
        .read_header(expected_oid)
        .map_err(error)?;
    if kind != git2::ObjectType::Blob {
        return Err(BridgeError::new(
            "repository_error",
            "Historical content is not a blob.",
        ));
    }
    scope.validate_submodules(&repo_path)?;
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != identity
    {
        return Err(stale());
    }
    Ok(json!({"size": size}))
}
