use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::SubmoduleConfig;

use super::{Group, LinkSnapshot, Request as InspectRequest, read_link};
use crate::bridge::{BridgeError, changes, scope::Scope};

#[cfg(test)]
mod tests;

const MAX_LINKS: usize = 32;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct Request {
    chain: Vec<Link>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    repo: PathBuf,
    path: PathBuf,
    group: Group,
    original_path: Option<PathBuf>,
    record: Value,
    child_repo: PathBuf,
    base_oid: Option<String>,
    head_oid: Option<String>,
    child_head_oid: String,
    parent_identity: String,
    child_identity: String,
}

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn group_name(group: Group) -> &'static str {
    match group {
        Group::Index => "index",
        Group::Working => "working",
        Group::Attention => "attention",
        Group::Merge => "merge",
    }
}

fn validate_link(link: &Link) -> Result<(), BridgeError> {
    if !link.repo.is_absolute()
        || !link.child_repo.is_absolute()
        || !relative(&link.path)
        || link
            .original_path
            .as_deref()
            .is_some_and(|path| !relative(path))
        || link.base_oid.as_deref().is_some_and(|value| !oid(value))
        || link.head_oid.as_deref().is_some_and(|value| !oid(value))
        || !oid(&link.child_head_oid)
        || link.parent_identity.len() != 64
        || !oid(&link.parent_identity)
        || link.child_identity.len() != 64
        || !oid(&link.child_identity)
    {
        return Err(invalid("invalid submodule chain link"));
    }
    let object = link
        .record
        .as_object()
        .ok_or_else(|| invalid("invalid submodule change record"))?;
    let path = link
        .path
        .to_str()
        .ok_or_else(|| invalid("invalid submodule path"))?;
    let original = link.original_path.as_ref().and_then(|path| path.to_str());
    let kind = object.get("kind").and_then(Value::as_str);
    let valid_kind = match link.group {
        Group::Index => matches!(
            kind,
            Some(
                "index_modified"
                    | "index_added"
                    | "index_deleted"
                    | "index_renamed"
                    | "index_copied"
                    | "index_type_changed"
            )
        ),
        Group::Working => matches!(
            kind,
            Some(
                "working_modified"
                    | "working_deleted"
                    | "working_renamed"
                    | "intent_to_add"
                    | "type_changed"
            )
        ),
        Group::Attention => kind == Some("submodule_attention"),
        Group::Merge => {
            return Err(invalid(
                "Conflicted submodule pointers cannot be inspected as staged or unstaged changes.",
            ));
        }
    };
    if object.len() != 6
        || !valid_kind
        || object.get("path").and_then(Value::as_str) != Some(path)
        || object.get("group").and_then(Value::as_str) != Some(group_name(link.group))
        || object.get("is_submodule") != Some(&Value::Bool(true))
        || object.get("conflict_stages") != Some(&Value::Null)
        || object.get("original_path") != Some(&json!(original))
    {
        return Err(invalid("invalid submodule change record"));
    }
    if link.original_path.is_some() {
        return Err(invalid(
            "Submodule renames or copies cannot be inspected yet. Inspect the change in Source Control.",
        ));
    }
    Ok(())
}

fn matches_expected(
    link: &Link,
    snapshot: &LinkSnapshot,
    expected_child: &Path,
) -> Result<bool, BridgeError> {
    Ok(snapshot.child == expected_child
        && snapshot.base.map(|oid| oid.to_string()) == link.base_oid
        && snapshot.head.map(|oid| oid.to_string()) == link.head_oid
        && snapshot.child_head.to_string() == link.child_head_oid
        && snapshot.parent_identity.fingerprint()? == link.parent_identity
        && snapshot.child_identity.fingerprint()? == link.child_identity)
}

fn read_current(
    scope: &Scope,
    link: &Link,
    config: &SubmoduleConfig,
) -> Result<Option<LinkSnapshot>, BridgeError> {
    let parent = scope.repository(&link.repo)?;
    let repository = git2::Repository::open(&parent)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let record = changes::current_record(
        &repository,
        config,
        link.path
            .to_str()
            .ok_or_else(|| invalid("invalid submodule path"))?,
        group_name(link.group),
    )?;
    if record.as_ref() != Some(&link.record) {
        return Ok(None);
    }
    let input = InspectRequest {
        repo: link.repo.clone(),
        path: link.path.clone(),
        summary_only: true,
        group: Some(link.group),
        original_path: link.original_path.clone(),
    };
    let snapshot = read_link(scope, &input)?;
    if snapshot.parent_path != parent {
        return Ok(None);
    }
    Ok(Some(snapshot))
}

/// Validate a contiguous chain without changing the caller's change snapshots.
pub(in crate::bridge) fn validate(
    scope: &Scope,
    input: Request,
    config: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    if input.chain.len() > MAX_LINKS {
        return Err(BridgeError::new(
            "chain_too_large",
            "Submodule chain exceeds 32 links.",
        ));
    }
    for link in &input.chain {
        validate_link(link)?;
    }
    let Some(first) = input.chain.first() else {
        return Ok(json!({"valid": true}));
    };
    let root = scope.repository(&first.repo)?;
    scope.validate_submodules(&root)?;
    let mut initial = Vec::with_capacity(input.chain.len());
    let mut previous_child: Option<PathBuf> = None;
    for link in &input.chain {
        if !scope.allows_missing(&link.child_repo)? {
            return Err(BridgeError::new(
                "outside_workspace",
                "Submodule is outside the workspace.",
            ));
        }
        let parent = scope.repository(&link.repo)?;
        if previous_child
            .as_ref()
            .is_some_and(|child| child != &parent)
        {
            return Err(invalid("submodule chain is not contiguous"));
        }
        let expected_child = scope.repository(&link.child_repo)?;
        let Some(snapshot) = read_current(scope, link, config)? else {
            return Ok(json!({"valid": false}));
        };
        if !matches_expected(link, &snapshot, &expected_child)? {
            return Ok(json!({"valid": false}));
        }
        previous_child = Some(expected_child);
        initial.push(snapshot);
    }
    scope.validate_submodules(&root)?;
    for (link, before) in input.chain.iter().zip(initial) {
        let expected_child = scope.repository(&link.child_repo)?;
        let Some(after) = read_current(scope, link, config)? else {
            return Ok(json!({"valid": false}));
        };
        if before != after || !matches_expected(link, &after, &expected_child)? {
            return Ok(json!({"valid": false}));
        }
    }
    scope.validate_submodules(&root)?;
    Ok(json!({"valid": true}))
}
