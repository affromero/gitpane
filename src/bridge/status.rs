use std::cell::RefCell;
use std::path::Path;

use serde_json::{Value, json};

use crate::config::SubmoduleConfig;
use crate::git::status::{FileStatus, query_status_scoped, validate_initialized_submodule};

use super::{BridgeError, scope::Scope};

fn utf8_path(path: &Path) -> Result<&str, BridgeError> {
    path.to_str()
        .ok_or_else(|| BridgeError::new("repository_error", "repository contains a non-UTF-8 path"))
}

pub(super) fn query(
    repo: &Path,
    scope: &Scope,
    sub_cfg: &SubmoduleConfig,
    include_worktrees: bool,
) -> Result<Value, BridgeError> {
    validate_submodules(scope, repo, sub_cfg)?;
    let common_repo = git2::Repository::open(repo)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let common_dir = common_repo
        .commondir()
        .canonicalize()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let failure = RefCell::new(None);
    let allow_worktree = |path: &Path| match scope.allows_missing(path) {
        Ok(false) => false,
        Ok(true) => match scope
            .repository(path)
            .and_then(|path| validate_submodules(scope, &path, sub_cfg))
        {
            Ok(()) => true,
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                false
            }
        },
        Err(error) => {
            *failure.borrow_mut() = Some(error);
            false
        }
    };
    let result = query_status_scoped(repo, sub_cfg, &allow_worktree, include_worktrees);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    let result = result.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;

    let files = result
        .files
        .iter()
        .map(|file| {
            let status = match file.status {
                FileStatus::Modified => "modified",
                FileStatus::Added => "added",
                FileStatus::Deleted => "deleted",
                FileStatus::Renamed => "renamed",
                FileStatus::Untracked => "untracked",
                FileStatus::Conflicted => "conflicted",
            };
            Ok(json!({
                "path": utf8_path(&file.path)?,
                "status": status,
                "staged": file.staged,
                "unstaged": file.unstaged,
                "is_submodule": file.is_submodule,
            }))
        })
        .collect::<Result<Vec<_>, BridgeError>>()?;
    let worktrees = result
        .worktree_info
        .iter()
        .map(|worktree| {
            Ok(json!({
                "path": utf8_path(&worktree.path)?,
                "branch": worktree.branch,
                "ahead": worktree.ahead,
                "behind": worktree.behind,
                "is_dirty": worktree.is_dirty,
                "file_count": worktree.file_count,
            }))
        })
        .collect::<Result<Vec<_>, BridgeError>>()?;
    let stashes: Vec<_> = result
        .stashes
        .iter()
        .map(|stash| json!({ "index": stash.index, "message": stash.message, "oid": stash.oid }))
        .collect();
    Ok(json!({
        "common_dir": utf8_path(&common_dir)?,
        "branch": result.branch,
        "head_oid": result.head_oid,
        "ahead": result.ahead,
        "behind": result.behind,
        "is_dirty": result.is_dirty,
        "files": files,
        "worktrees": worktrees,
        "has_submodules": result.has_submodules,
        "has_dirty_submodules": result.has_dirty_submodules,
        "has_unpushed_submodules": result.has_unpushed_submodules,
        "stashes": stashes,
    }))
}

fn validate_submodules(
    scope: &Scope,
    repo: &Path,
    sub_cfg: &SubmoduleConfig,
) -> Result<(), BridgeError> {
    for child in scope.submodule_repositories(repo)? {
        validate_initialized_submodule(&child, sub_cfg).map_err(|error| {
            BridgeError::new(
                "repository_error",
                format!("submodule {}: {error}", child.display()),
            )
        })?;
    }
    Ok(())
}
