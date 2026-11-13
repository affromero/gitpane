use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{head_filemode, registered_link, valid_submodule_path};
use crate::bridge::{BridgeError, scope::Scope};

#[cfg(test)]
mod tests;

const MAX_LINKS: usize = 32;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct Request {
    root: PathBuf,
    chain: Vec<Link>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    path: PathBuf,
    parent_identity: String,
    child_identity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct DiscoverRequest {
    repo: PathBuf,
    path: PathBuf,
    expected_parent_identity: String,
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Pinned submodule registration or repository changed; pin it again.",
    )
}

fn has_gitlink(parent: &Path, path: &Path) -> Result<bool, BridgeError> {
    let repo = git2::Repository::open(parent)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    if head_filemode(&repo, path)?.is_some_and(|mode| mode & 0o170000 == 0o160000) {
        return Ok(true);
    }
    let index = repo
        .index()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    Ok((0..=3).any(|stage| {
        index
            .get_path(path, stage)
            .is_some_and(|entry| entry.mode & 0o170000 == 0o160000)
    }))
}

/// List initialized, registered children with identities suitable for a pin chain.
pub(in crate::bridge) fn list(scope: &Scope, repo: &Path) -> Result<Value, BridgeError> {
    let root = scope.repository(repo)?;
    scope.validate_modules_file(&root)?;
    let parent = git2::Repository::open(&root)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let modules = parent
        .submodules()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let mut children = Vec::new();
    for module in &modules {
        let path = module.path();
        if !valid_submodule_path(path) || !has_gitlink(&root, path)? {
            continue;
        }
        super::require_pin_path(&root, path)?;
        match root.join(path).join(".git").symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(BridgeError::new("repository_error", error.to_string())),
        }
        let link = registered_link(scope, &root, path, true)?;
        children.push(json!({
            "path": path,
            "repo": link.child,
            "parent_identity": link.parent_identity.fingerprint()?,
            "child_identity": link.child_identity.fingerprint()?,
        }));
    }
    Ok(json!({ "submodules": children }))
}

pub(in crate::bridge) fn discover(
    scope: &Scope,
    input: DiscoverRequest,
) -> Result<Value, BridgeError> {
    if !input.repo.is_absolute()
        || !valid_submodule_path(&input.path)
        || !valid_identity(&input.expected_parent_identity)
    {
        return Err(BridgeError::new(
            "invalid_request",
            "Invalid submodule discovery request.",
        ));
    }
    let read = || {
        let link = registered_link(scope, &input.repo, &input.path, true)?;
        if link.parent_identity.fingerprint()? != input.expected_parent_identity
            || !has_gitlink(&link.parent_path, &input.path)?
        {
            return Err(stale());
        }
        Ok((link.child, link.child_identity.fingerprint()?))
    };
    let initial = read()?;
    if read()? != initial {
        return Err(stale());
    }
    Ok(
        json!({"repo": initial.0, "parent_identity": input.expected_parent_identity,
        "child_identity": initial.1}),
    )
}

fn walk(scope: &Scope, root: &Path, chain: &[Link]) -> Result<Vec<PathBuf>, BridgeError> {
    let root = scope.repository(root)?;
    let mut seen = HashSet::from([root.clone()]);
    let mut repos = vec![root];
    for link in chain {
        let parent = repos.last().expect("root is present");
        let current = registered_link(scope, parent, &link.path, true).map_err(|error| {
            if error.code == "invalid_request" {
                stale()
            } else {
                error
            }
        })?;
        if !has_gitlink(parent, &link.path)? {
            return Err(stale());
        }
        if current.parent_path != *parent
            || current.parent_identity.fingerprint()? != link.parent_identity
            || current.child_identity.fingerprint()? != link.child_identity
        {
            return Err(stale());
        }
        if !seen.insert(current.child.clone()) {
            return Err(BridgeError::new(
                "invalid_request",
                "Pinned submodule chain contains a cycle.",
            ));
        }
        repos.push(current.child);
    }
    Ok(repos)
}

pub(in crate::bridge) fn validate(scope: &Scope, input: Request) -> Result<Value, BridgeError> {
    if !input.root.is_absolute() || input.chain.is_empty() {
        return Err(BridgeError::new(
            "invalid_request",
            "A workspace repository and nonempty submodule chain are required.",
        ));
    }
    if input.chain.len() > MAX_LINKS {
        return Err(BridgeError::new(
            "chain_too_large",
            "Submodule chain exceeds 32 links.",
        ));
    }
    if input.chain.iter().any(|link| {
        !valid_submodule_path(&link.path)
            || !valid_identity(&link.parent_identity)
            || !valid_identity(&link.child_identity)
    }) {
        return Err(BridgeError::new(
            "invalid_request",
            "Invalid pinned submodule chain link.",
        ));
    }
    let initial = walk(scope, &input.root, &input.chain)?;
    let current = walk(scope, &input.root, &input.chain)?;
    if initial != current {
        return Err(stale());
    }
    Ok(json!({"repos": current}))
}
