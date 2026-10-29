use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::bridge::{
    BridgeError,
    changes::{self, RepositoryIdentity},
    scope::Scope,
};
use crate::config::SubmoduleConfig;

use super::valid_submodule_path;

#[cfg(test)]
mod tests;

const MAX_CONTENT: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct CompareRequest {
    repo: PathBuf,
    record: Value,
    expected_repo_identity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct ContentRequest {
    repo: PathBuf,
    record: Value,
    expected_repo_identity: String,
    head_oid: Option<String>,
    sides: Value,
    side: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct BatchRequest {
    repo: PathBuf,
    expected_repo_identity: String,
    snapshots: Vec<ExpectedSnapshot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedSnapshot {
    record: Value,
    head_oid: Option<String>,
    sides: Value,
}

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}
fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Submodule pointer changed; refresh before opening the diff.",
    )
}
fn repository_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn entry(path: &str, mode: u32, oid: git2::Oid) -> Result<Value, BridgeError> {
    if !valid_submodule_path(Path::new(path)) {
        return Err(invalid("Invalid submodule pointer path."));
    }
    let kind = match mode & 0o170000 {
        0o160000 => "gitlink",
        0o100000 => "blob",
        0o120000 => "symlink",
        0o040000 => {
            return Err(invalid(
                "A directory side cannot be compared as a submodule pointer.",
            ));
        }
        _ => return Err(invalid("Unsupported submodule pointer side.")),
    };
    if oid.is_zero() {
        return Err(repository_error(
            "Submodule pointer has an invalid object ID.",
        ));
    }
    Ok(json!({"path": path, "mode": mode, "oid": oid.to_string(), "kind": kind}))
}

fn index_entry(index: &git2::Index, path: Option<&str>, stage: i32) -> Result<Value, BridgeError> {
    match path.and_then(|path| {
        index
            .get_path(Path::new(path), stage)
            .map(|item| (path, item))
    }) {
        Some((path, item)) => entry(path, item.mode, item.id),
        None if path.is_none() => Ok(Value::Null),
        None => Err(stale()),
    }
}

fn record_key(record: &Value) -> Result<(&str, &str), BridgeError> {
    let path = record
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Invalid change record."))?;
    let group = record
        .get("group")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Invalid change record."))?;
    if !valid_submodule_path(Path::new(path))
        || !matches!(group, "index" | "merge")
        || record.get("is_submodule") != Some(&Value::Bool(true))
    {
        return Err(invalid("Expected a staged or conflicted submodule change."));
    }
    Ok((path, group))
}

fn read_head(repo: &git2::Repository) -> Result<Option<git2::Commit<'_>>, BridgeError> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit().map_err(repository_error)?)),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(repository_error(error)),
    }
}

fn snapshot(
    scope: &Scope,
    request: &CompareRequest,
    cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    if !identity(&request.expected_repo_identity) {
        return Err(invalid("Invalid repository identity."));
    }
    let (path, group) = record_key(&request.record)?;
    let repo_path = scope.repository(&request.repo)?;
    scope.validate_submodules(&repo_path)?;
    let first_identity = RepositoryIdentity::read(&repo_path)?;
    if first_identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path).map_err(repository_error)?;
    if changes::current_record(&repo, cfg, path, group)?.as_ref() != Some(&request.record) {
        return Err(stale());
    }
    let head = read_head(&repo)?;
    let index = repo.index().map_err(repository_error)?;
    let result = snapshot_fields(
        &index,
        &head,
        &request.record,
        &request.expected_repo_identity,
    )?;
    scope.validate_submodules(&repo_path)?;
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != first_identity
    {
        return Err(stale());
    }
    Ok(result)
}

fn snapshot_fields(
    index: &git2::Index,
    head: &Option<git2::Commit<'_>>,
    record: &Value,
    expected_identity: &str,
) -> Result<Value, BridgeError> {
    let (path, group) = record_key(record)?;
    let head_oid = head.as_ref().map(|commit| commit.id().to_string());
    let sides = if group == "index" {
        let old = record
            .get("original_path")
            .and_then(Value::as_str)
            .unwrap_or(path);
        if !valid_submodule_path(Path::new(old)) {
            return Err(invalid("Invalid original path."));
        }
        let base = match head.as_ref() {
            Some(commit) => match commit
                .tree()
                .map_err(repository_error)?
                .get_path(Path::new(old))
            {
                Ok(item) => entry(old, item.filemode() as u32, item.id())?,
                Err(error) if error.code() == git2::ErrorCode::NotFound => Value::Null,
                Err(error) => return Err(repository_error(error)),
            },
            None => Value::Null,
        };
        let current = match index.get_path(Path::new(path), 0) {
            Some(item) => entry(path, item.mode, item.id)?,
            None => Value::Null,
        };
        json!({"base":base,"index":current})
    } else {
        let stages = record
            .get("conflict_stages")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("Invalid conflict stages."))?;
        let stage_path = |name: &str| -> Result<Option<&str>, BridgeError> {
            match stages.get(name) {
                Some(Value::Null) => Ok(None),
                Some(Value::String(path)) if valid_submodule_path(Path::new(path)) => {
                    Ok(Some(path))
                }
                _ => Err(invalid("Invalid conflict stage path.")),
            }
        };
        json!({"base":index_entry(index,stage_path("base")?,1)?,
            "ours":index_entry(index,stage_path("ours")?,2)?,
            "theirs":index_entry(index,stage_path("theirs")?,3)?})
    };
    Ok(json!({"repo_identity":expected_identity,"head_oid":head_oid,"record":record,"sides":sides}))
}

fn batch_pass(
    scope: &Scope,
    request: &BatchRequest,
    cfg: &SubmoduleConfig,
) -> Result<Vec<Result<Value, BridgeError>>, BridgeError> {
    let repo_path = scope.repository(&request.repo)?;
    scope.validate_submodules(&repo_path)?;
    let before = RepositoryIdentity::read(&repo_path)?;
    if before.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path).map_err(repository_error)?;
    let keys: HashSet<_> = request
        .snapshots
        .iter()
        .filter_map(|item| {
            record_key(&item.record)
                .ok()
                .map(|(path, group)| (path.to_owned(), group.to_owned()))
        })
        .collect();
    let records = changes::current_records(&repo, cfg, &keys)?;
    let head = read_head(&repo)?;
    let index = repo.index().map_err(repository_error)?;
    let results = request
        .snapshots
        .iter()
        .map(|item| {
            let (path, group) = record_key(&item.record)?;
            if records.get(&(path.to_owned(), group.to_owned())) != Some(&item.record) {
                return Err(stale());
            }
            snapshot_fields(&index, &head, &item.record, &request.expected_repo_identity)
        })
        .collect();
    scope.validate_submodules(&repo_path)?;
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != before
    {
        return Err(stale());
    }
    Ok(results)
}

pub(in crate::bridge) fn validate_batch(
    scope: &Scope,
    request: BatchRequest,
    cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    if !identity(&request.expected_repo_identity)
        || request.snapshots.is_empty()
        || request.snapshots.len() > 128
    {
        return Err(invalid(
            "A valid repository identity and 1 to 128 pointer snapshots are required.",
        ));
    }
    let first = batch_pass(scope, &request, cfg)?;
    let second = batch_pass(scope, &request, cfg)?;
    let results: Vec<_> = request.snapshots.iter().zip(first).zip(second).map(|((item, first), second)| {
        let result = match (first, second) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(first), Ok(second)) => {
                let expected = json!({"repo_identity":request.expected_repo_identity,"head_oid":item.head_oid,"record":item.record,"sides":item.sides});
                if first == expected && second == expected { Ok(()) } else { Err(stale()) }
            }
        };
        match result {
            Ok(()) => json!({"valid":true}),
            Err(error) => json!({"valid":false,"code":error.code,"message":error.message}),
        }
    }).collect();
    Ok(json!({"results":results}))
}

pub(in crate::bridge) fn compare(
    scope: &Scope,
    request: CompareRequest,
    cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    let initial = snapshot(scope, &request, cfg)?;
    if snapshot(scope, &request, cfg)? != initial {
        return Err(stale());
    }
    Ok(initial)
}

pub(in crate::bridge) fn content(
    scope: &Scope,
    request: ContentRequest,
    cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    let compare_request = CompareRequest {
        repo: request.repo,
        record: request.record,
        expected_repo_identity: request.expected_repo_identity,
    };
    let expected = json!({"repo_identity":compare_request.expected_repo_identity,"head_oid":request.head_oid,"record":compare_request.record,"sides":request.sides});
    let initial = snapshot(scope, &compare_request, cfg)?;
    if initial != expected {
        return Err(stale());
    }
    let side = initial["sides"]
        .get(&request.side)
        .ok_or_else(|| invalid("Invalid pointer side."))?;
    let content = if side.is_null() {
        String::new()
    } else {
        let oid = side["oid"].as_str().ok_or_else(stale)?;
        if side["kind"] == "gitlink" {
            format!("Subproject commit {oid}\n")
        } else {
            let repo_path = scope.repository(&compare_request.repo)?;
            let repo = git2::Repository::open(repo_path).map_err(repository_error)?;
            let oid = git2::Oid::from_str(oid).map_err(repository_error)?;
            let (size, kind) = repo
                .odb()
                .map_err(repository_error)?
                .read_header(oid)
                .map_err(repository_error)?;
            if kind != git2::ObjectType::Blob {
                return Err(repository_error("Pointer side is not a blob."));
            }
            if size > MAX_CONTENT {
                return Err(invalid("Pointer side exceeds the 1 MiB text limit."));
            }
            let blob = repo.find_blob(oid).map_err(repository_error)?;
            if blob.size() > MAX_CONTENT {
                return Err(invalid("Pointer side exceeds the 1 MiB text limit."));
            }
            let bytes = blob.content();
            if bytes.contains(&0) {
                return Err(invalid("Binary pointer sides cannot be opened as text."));
            }
            std::str::from_utf8(bytes)
                .map_err(|_| invalid("Pointer side is not UTF-8 text."))?
                .to_owned()
        }
    };
    if snapshot(scope, &compare_request, cfg)? != initial {
        return Err(stale());
    }
    Ok(json!({"content":content}))
}
