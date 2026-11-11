use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::git::stash::{self, StashSource};

use super::{BridgeError, changes::RepositoryIdentity, scope::Scope};

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct FilesRequest {
    repo: PathBuf,
    oid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct ContentRequest {
    repo: PathBuf,
    oid: String,
    expected_repo_identity: String,
    source: StashSource,
    path: String,
}

fn valid_oid(oid: &str) -> bool {
    oid.len() == 40 && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn repository_error(error: color_eyre::Report) -> BridgeError {
    let code = if error
        .downcast_ref::<git2::Error>()
        .is_some_and(|error| error.code() == git2::ErrorCode::NotFound)
    {
        "content_not_found"
    } else {
        "repository_error"
    };
    BridgeError::new(code, error.to_string())
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Repository changed while reading the stash snapshot; refresh and try again.",
    )
}

fn response(value: impl Serialize, identity: &RepositoryIdentity) -> Result<Value, BridgeError> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    value["repo_identity"] = json!(identity.fingerprint()?);
    Ok(value)
}

pub(in crate::bridge) fn files(scope: &Scope, request: FilesRequest) -> Result<Value, BridgeError> {
    if !valid_oid(&request.oid) {
        return Err(BridgeError::new(
            "invalid_request",
            "A full 40-character stash object ID is required.",
        ));
    }
    let repo = scope.repository(&request.repo)?;
    let identity = RepositoryIdentity::read(&repo)?;
    let files = stash::list_stash_files(&repo, &request.oid).map_err(repository_error)?;
    if scope.repository(&request.repo)? != repo || RepositoryIdentity::read(&repo)? != identity {
        return Err(stale());
    }
    response(files, &identity)
}

pub(in crate::bridge) fn content(
    scope: &Scope,
    request: ContentRequest,
) -> Result<Value, BridgeError> {
    if !valid_oid(&request.oid)
        || request.expected_repo_identity.len() != 64
        || !request
            .expected_repo_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !super::changes::blob::valid_path(&request.path)
    {
        return Err(BridgeError::new(
            "invalid_request",
            "A stash object ID, repository identity, source and literal relative path are required.",
        ));
    }
    let repo = scope.repository(&request.repo)?;
    let identity = RepositoryIdentity::read(&repo)?;
    if identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let content = stash::stash_file_content(&repo, &request.oid, request.source, &request.path)
        .map_err(repository_error)?;
    if scope.repository(&request.repo)? != repo || RepositoryIdentity::read(&repo)? != identity {
        return Err(stale());
    }
    let mut value = response(content, &identity)?;
    // The extension compares the saved sides itself and does not use the patch.
    value
        .as_object_mut()
        .expect("stash content is an object")
        .remove("diff");
    Ok(value)
}
