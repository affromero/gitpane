use std::path::{Component, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::SubmoduleConfig;
use crate::git::commit_files::list_revision_files;

use super::{BridgeError, commit_file_json, scope::Scope, status};

pub(super) mod actions;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    repo: PathBuf,
    path: PathBuf,
}

fn error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

pub(super) fn inspect(
    scope: &Scope,
    input: Request,
    sub_cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    if input.path.as_os_str().is_empty()
        || input
            .path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(BridgeError::new(
            "invalid_request",
            "submodule path must be relative",
        ));
    }
    let parent_path = scope.repository(&input.repo)?;
    scope.validate_submodules(&parent_path)?;
    let parent = git2::Repository::open(&parent_path).map_err(error)?;
    let modules = parent.submodules().map_err(error)?;
    let sub = modules
        .iter()
        .find(|sub| sub.path() == input.path)
        .ok_or_else(|| {
            BridgeError::new(
                "invalid_request",
                "selected path is not a registered submodule",
            )
        })?;
    let child_path = parent_path.join(&input.path);
    if !scope.allows_missing(&child_path)? {
        return Err(BridgeError::new(
            "outside_workspace",
            "submodule is outside the workspace",
        ));
    }
    let child = scope.repository(&child_path).map_err(|failure| {
        if failure.code == "repository_error"
            || failure.message == "path does not identify a workspace repository root"
        {
            BridgeError::new(
                "repository_error",
                format!(
                    "Submodule is not initialized or unavailable: {}",
                    failure.message
                ),
            )
        } else {
            failure
        }
    })?;
    scope.validate_submodules(&child)?;
    let child_repo = git2::Repository::open(&child).map_err(error)?;
    let head = child_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(error)?
        .id();
    let base = sub.head_id();
    let index = sub.index_id();
    let files = if base == Some(head) {
        Vec::new()
    } else {
        list_revision_files(&child, base, head).map_err(error)?
    };
    let child_status = status::query(&child, scope, sub_cfg)?;
    if scope.repository(&input.repo)? != parent_path || scope.repository(&child_path)? != child {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    let parent_again = git2::Repository::open(&parent_path).map_err(error)?;
    let modules_again = parent_again.submodules().map_err(error)?;
    let current = modules_again.iter().find(|sub| sub.path() == input.path);
    let head_again = child_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(error)?
        .id();
    if current.is_none_or(|sub| sub.head_id() != base || sub.index_id() != index)
        || head_again != head
    {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    let child_text = child
        .to_str()
        .ok_or_else(|| error("submodule path is not valid UTF-8"))?;
    Ok(json!({
        "repo": child_text,
        "base_oid": base.map(|id| id.to_string()),
        "head_oid": head.to_string(),
        "status": child_status,
        "pointer_files": files.into_iter().map(commit_file_json).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::path::Path;

    fn commit_file(repo: &Repository, name: &str, text: &str) -> git2::Oid {
        std::fs::write(repo.workdir().unwrap().join(name), text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.invalid").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "test", &tree, &parents)
            .unwrap()
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Scope) {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let parent_path = workspace.join("parent");
        let source_path = temporary.path().join("source");
        std::fs::create_dir_all(&parent_path).unwrap();
        std::fs::create_dir_all(&source_path).unwrap();
        let source = Repository::init(&source_path).unwrap();
        commit_file(&source, "base", "base");
        let parent = Repository::init(&parent_path).unwrap();
        commit_file(&parent, "parent", "parent");
        let mut sub = parent
            .submodule(source_path.to_str().unwrap(), Path::new("child"), true)
            .unwrap();
        sub.clone(None).unwrap();
        sub.add_finalize().unwrap();
        drop(sub);
        commit_file(&parent, "parent", "parent");
        let scope = Scope::new(vec![workspace]).unwrap();
        (
            temporary,
            parent_path.clone(),
            parent_path.join("child"),
            scope,
        )
    }

    fn request(repo: &std::path::Path, path: &str, scope: &Scope) -> Result<Value, BridgeError> {
        inspect(
            scope,
            Request {
                repo: repo.to_path_buf(),
                path: PathBuf::from(path),
            },
            &SubmoduleConfig::default(),
        )
    }

    #[test]
    fn inspect_returns_scoped_dirty_status_and_pointer_changes() {
        let (_temp, parent, child, scope) = fixture();
        let child_repo = Repository::open(&child).unwrap();
        let base = child_repo.head().unwrap().peel_to_commit().unwrap().id();
        let head = commit_file(&child_repo, "committed", "new");
        std::fs::write(child.join("base"), "dirty").unwrap();
        let result = request(&parent, "child", &scope).unwrap();
        assert_eq!(result["base_oid"], base.to_string());
        assert_eq!(result["head_oid"], head.to_string());
        assert_eq!(result["status"]["is_dirty"], true);
        assert!(
            result["status"]["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["path"] == "base")
        );
        assert_eq!(
            result["pointer_files"],
            json!([{"status": "A", "path": "committed", "is_submodule": false}])
        );
        assert_eq!(
            request(&parent, "../source", &scope).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(
            request(&parent, "missing", &scope).unwrap_err().code,
            "invalid_request"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspect_rejects_outside_and_uninitialized_worktrees() {
        let (temp, parent, child, scope) = fixture();
        let saved = temp.path().join("saved");
        std::fs::rename(&child, &saved).unwrap();
        let missing = request(&parent, "child", &scope).unwrap_err();
        assert!(
            missing.message.contains("not initialized"),
            "{}",
            missing.message
        );
        std::os::unix::fs::symlink(temp.path().join("source"), &child).unwrap();
        assert_eq!(
            request(&parent, "child", &scope).unwrap_err().code,
            "outside_workspace"
        );
    }
}
