use std::path::{Component, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::SubmoduleConfig;
use crate::git::commit_files::list_revision_files;

use super::{BridgeError, changes::RepositoryIdentity, commit_file_json, scope::Scope, status};

pub(super) mod actions;

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Group {
    Index,
    Working,
    Attention,
    Merge,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    repo: PathBuf,
    path: PathBuf,
    #[serde(default)]
    summary_only: bool,
    #[serde(default)]
    group: Option<Group>,
    #[serde(default)]
    original_path: Option<PathBuf>,
}

fn error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn ensure_no_conflict(repo: &git2::Repository, path: &std::path::Path) -> Result<(), BridgeError> {
    let index = repo.index().map_err(error)?;
    if (1..=3).any(|stage| index.get_path(path, stage).is_some()) {
        return Err(BridgeError::new(
            "invalid_request",
            "Conflicted submodule pointers cannot be inspected as staged or unstaged changes.",
        ));
    }
    Ok(())
}

fn ensure_no_type_change(
    repo: &git2::Repository,
    path: &std::path::Path,
) -> Result<(), BridgeError> {
    let head_mode = match repo.head() {
        Ok(head) => {
            let tree = head.peel_to_tree().map_err(error)?;
            match tree.get_path(path) {
                Ok(entry) => Some(entry.filemode() as u32),
                Err(failure) if failure.code() == git2::ErrorCode::NotFound => None,
                Err(failure) => return Err(error(failure)),
            }
        }
        Err(failure)
            if matches!(
                failure.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            None
        }
        Err(failure) => return Err(error(failure)),
    };
    let index_mode = repo
        .index()
        .map_err(error)?
        .get_path(path, 0)
        .map(|entry| entry.mode);
    if let (Some(head_mode), Some(index_mode)) = (head_mode, index_mode)
        && (head_mode & 0o170000 == 0o160000) != (index_mode & 0o170000 == 0o160000)
    {
        return Err(BridgeError::new(
            "invalid_request",
            "Submodule type changes cannot be inspected here. Inspect both sides in Source Control.",
        ));
    }
    Ok(())
}

fn ensure_gitlink_index(
    repo: &git2::Repository,
    path: &std::path::Path,
) -> Result<(), BridgeError> {
    let index = repo.index().map_err(error)?;
    if index
        .get_path(path, 0)
        .is_some_and(|entry| entry.mode & 0o170000 != 0o160000)
    {
        return Err(BridgeError::new(
            "invalid_request",
            "Submodule type changes cannot be inspected here. Inspect both sides in Source Control.",
        ));
    }
    Ok(())
}

fn ensure_supported_type(
    repo: &git2::Repository,
    path: &std::path::Path,
    group: Option<Group>,
) -> Result<(), BridgeError> {
    if matches!(group, Some(Group::Working | Group::Attention)) {
        ensure_gitlink_index(repo, path)
    } else {
        ensure_no_type_change(repo, path)
    }
}

pub(super) fn inspect(
    scope: &Scope,
    input: Request,
    sub_cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    if input.path.as_os_str().is_empty()
        || input.path.as_os_str().as_encoded_bytes().contains(&0)
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
    if input.group == Some(Group::Merge) {
        return Err(BridgeError::new(
            "invalid_request",
            "Conflicted submodule pointers cannot be inspected as staged or unstaged changes.",
        ));
    }
    if let Some(original) = &input.original_path {
        if input.group != Some(Group::Index)
            || original.as_os_str().is_empty()
            || original.as_os_str().as_encoded_bytes().contains(&0)
            || original
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(BridgeError::new(
                "invalid_request",
                "invalid original submodule path",
            ));
        }
        return Err(BridgeError::new(
            "invalid_request",
            "Staged submodule renames cannot be inspected yet. Inspect the rename in Source Control.",
        ));
    }
    let parent_identity = RepositoryIdentity::read(&parent_path)?;
    let parent = git2::Repository::open(&parent_path).map_err(error)?;
    ensure_no_conflict(&parent, &input.path)?;
    ensure_supported_type(&parent, &input.path, input.group)?;
    let modules = parent.submodules().map_err(error)?;
    let sub = modules
        .iter()
        .find(|sub| sub.path() == input.path)
        .ok_or_else(|| {
            BridgeError::new(
                "invalid_request",
                "Submodule is no longer registered or unavailable.",
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
    let child_identity = RepositoryIdentity::read(&child)?;
    let child_repo = git2::Repository::open(&child).map_err(error)?;
    let child_head = child_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(error)?
        .id();
    let parent_head = sub.head_id();
    let index = sub.index_id();
    let (base, head) = match input.group {
        Some(Group::Index) => (parent_head, index),
        Some(Group::Working | Group::Attention) => (index, Some(child_head)),
        None => (parent_head, Some(child_head)),
        Some(Group::Merge) => unreachable!(),
    };
    let files = if base == head {
        Vec::new()
    } else {
        list_revision_files(&child, base, head).map_err(error)?
    };
    let child_status = status::query(&child, scope, sub_cfg, true, input.summary_only, &[])?;
    if scope.repository(&input.repo)? != parent_path || scope.repository(&child_path)? != child {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    scope.validate_submodules(&parent_path)?;
    scope.validate_submodules(&child)?;
    if RepositoryIdentity::read(&parent_path)? != parent_identity
        || RepositoryIdentity::read(&child)? != child_identity
    {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    let parent_again = git2::Repository::open(&parent_path).map_err(error)?;
    ensure_no_conflict(&parent_again, &input.path).map_err(|failure| {
        if failure.code == "invalid_request" {
            BridgeError::new(
                "stale_target",
                "Submodule changed; refresh before inspecting",
            )
        } else {
            failure
        }
    })?;
    ensure_supported_type(&parent_again, &input.path, input.group).map_err(|failure| {
        if failure.code == "invalid_request" {
            BridgeError::new(
                "stale_target",
                "Submodule changed; refresh before inspecting",
            )
        } else {
            failure
        }
    })?;
    let modules_again = parent_again.submodules().map_err(error)?;
    let current = modules_again.iter().find(|sub| sub.path() == input.path);
    let head_again = git2::Repository::open(&child)
        .map_err(error)?
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(error)?
        .id();
    if current.is_none_or(|sub| sub.head_id() != parent_head || sub.index_id() != index)
        || head_again != child_head
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
        "head_oid": head.map(|id| id.to_string()),
        "child_head_oid": child_head.to_string(),
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
        request_group(repo, path, scope, Group::Working)
    }

    fn request_group(
        repo: &std::path::Path,
        path: &str,
        scope: &Scope,
        group: Group,
    ) -> Result<Value, BridgeError> {
        inspect(
            scope,
            Request {
                repo: repo.to_path_buf(),
                path: PathBuf::from(path),
                summary_only: false,
                group: Some(group),
                original_path: None,
            },
            &SubmoduleConfig::default(),
        )
    }

    #[test]
    fn index_and_worktree_select_distinct_gitlink_comparisons() {
        let (_temp, parent, child, scope) = fixture();
        let parent_repo = Repository::open(&parent).unwrap();
        let child_repo = Repository::open(&child).unwrap();
        let first = child_repo.head().unwrap().peel_to_commit().unwrap().id();
        let staged = commit_file(&child_repo, "staged.txt", "staged");
        let mut sub = parent_repo.find_submodule("child").unwrap();
        sub.reload(true).unwrap();
        sub.add_to_index(true).unwrap();
        let working = commit_file(&child_repo, "working.txt", "working");

        let index = request_group(&parent, "child", &scope, Group::Index).unwrap();
        assert_eq!(index["base_oid"], first.to_string());
        assert_eq!(index["head_oid"], staged.to_string());
        assert_eq!(index["child_head_oid"], working.to_string());
        assert_eq!(
            index["pointer_files"],
            json!([
                {"status": "A", "path": "staged.txt", "is_submodule": false}
            ])
        );
        let worktree = request_group(&parent, "child", &scope, Group::Working).unwrap();
        assert_eq!(worktree["base_oid"], staged.to_string());
        assert_eq!(worktree["head_oid"], working.to_string());
        assert_eq!(worktree["child_head_oid"], working.to_string());
        assert_eq!(
            worktree["pointer_files"],
            json!([
                {"status": "A", "path": "working.txt", "is_submodule": false}
            ])
        );
        let attention = request_group(&parent, "child", &scope, Group::Attention).unwrap();
        assert_eq!(attention["base_oid"], staged.to_string());
        assert_eq!(attention["head_oid"], working.to_string());
        let legacy = inspect(
            &scope,
            Request {
                repo: parent.clone(),
                path: PathBuf::from("child"),
                summary_only: false,
                group: None,
                original_path: None,
            },
            &SubmoduleConfig::default(),
        )
        .unwrap();
        assert_eq!(legacy["base_oid"], first.to_string());
        assert_eq!(legacy["head_oid"], working.to_string());
        assert_eq!(
            legacy["pointer_files"],
            json!([
                {"status": "A", "path": "staged.txt", "is_submodule": false},
                {"status": "A", "path": "working.txt", "is_submodule": false}
            ])
        );

        sub.reload(true).unwrap();
        sub.add_to_index(true).unwrap();
        let refreshed_index = request_group(&parent, "child", &scope, Group::Index).unwrap();
        assert_ne!(index["head_oid"], refreshed_index["head_oid"]);
        let refreshed_worktree = request_group(&parent, "child", &scope, Group::Working).unwrap();
        assert_eq!(refreshed_worktree["base_oid"], working.to_string());
        assert_eq!(refreshed_worktree["pointer_files"], json!([]));
        let clean_attention = request_group(&parent, "child", &scope, Group::Attention).unwrap();
        assert_eq!(clean_attention["pointer_files"], json!([]));
        let next = commit_file(&child_repo, "later.txt", "later");
        let later = request_group(&parent, "child", &scope, Group::Working).unwrap();
        assert_eq!(later["child_head_oid"], next.to_string());
        assert_ne!(
            refreshed_worktree["child_head_oid"],
            later["child_head_oid"]
        );
    }

    #[test]
    fn staged_addition_and_deletion_compare_against_empty_trees() {
        let (_temp, parent, _child, scope) = fixture();
        let parent_repo = Repository::open(&parent).unwrap();
        let mut index = parent_repo.index().unwrap();
        index.remove_path(Path::new("child")).unwrap();
        index.write().unwrap();
        let deleted = request_group(&parent, "child", &scope, Group::Index).unwrap();
        assert!(deleted["base_oid"].is_string());
        assert!(deleted["head_oid"].is_null());
        assert_eq!(
            deleted["pointer_files"],
            json!([
                {"status": "D", "path": "base", "is_submodule": false}
            ])
        );
        let working = request_group(&parent, "child", &scope, Group::Working).unwrap();
        assert!(working["base_oid"].is_null());
        assert_eq!(working["head_oid"], working["child_head_oid"]);
        assert_eq!(
            working["pointer_files"],
            json!([
                {"status": "A", "path": "base", "is_submodule": false}
            ])
        );

        commit_file(&parent_repo, "parent", "parent");
        let mut sub = parent_repo.find_submodule("child").unwrap();
        sub.reload(true).unwrap();
        sub.add_to_index(true).unwrap();
        let added = request_group(&parent, "child", &scope, Group::Index).unwrap();
        assert!(added["base_oid"].is_null());
        assert!(added["head_oid"].is_string());
        assert_eq!(
            added["pointer_files"],
            json!([
                {"status": "A", "path": "base", "is_submodule": false}
            ])
        );
    }

    #[test]
    fn staged_type_changes_report_an_actionable_error_in_both_directions() {
        let (_temp, parent, _child, scope) = fixture();
        let parent_repo = Repository::open(&parent).unwrap();
        let mut index = parent_repo.index().unwrap();
        let mut entry = index.get_path(Path::new("child"), 0).unwrap();
        let gitlink_oid = entry.id;
        entry.mode = 0o100644;
        entry.id = parent_repo.blob(b"replacement file").unwrap();
        index.add(&entry).unwrap();
        index.write().unwrap();
        for group in [Group::Index, Group::Working, Group::Attention] {
            let error = request_group(&parent, "child", &scope, group).unwrap_err();
            assert_eq!(error.code, "invalid_request");
            assert!(error.message.contains("type changes cannot be inspected"));
            assert!(error.message.contains("Source Control"));
        }

        commit_file(&parent_repo, "parent", "parent");
        entry.mode = 0o160000;
        entry.id = gitlink_oid;
        index.add(&entry).unwrap();
        index.write().unwrap();
        let error = request_group(&parent, "child", &scope, Group::Index).unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("type changes cannot be inspected"));
    }

    #[test]
    fn conflicted_gitlink_is_not_misrepresented_as_a_deletion() {
        let (_temp, parent, _child, scope) = fixture();
        let parent_repo = Repository::open(&parent).unwrap();
        let mut index = parent_repo.index().unwrap();
        let mut ancestor = index.get_path(Path::new("child"), 0).unwrap();
        ancestor.flags = (ancestor.flags & !(3 << 12)) | (1 << 12);
        index.remove_path(Path::new("child")).unwrap();
        index.add(&ancestor).unwrap();
        index.write().unwrap();
        let error = request_group(&parent, "child", &scope, Group::Merge).unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("Conflicted submodule"));
    }

    #[test]
    fn staged_submodule_rename_fails_explicitly_instead_of_comparing_with_empty() {
        if !crate::git::git_test_available() {
            return;
        }
        let (_temp, parent, _child, scope) = fixture();
        let output = crate::git::process::git_command(&parent)
            .args(["mv", "--", "child", "renamed"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let repo = Repository::open(&parent).unwrap();
        let index = repo.index().unwrap();
        assert!(index.get_path(Path::new("child"), 0).is_none());
        assert_eq!(
            index.get_path(Path::new("renamed"), 0).unwrap().mode,
            0o160000
        );
        let error = inspect(
            &scope,
            Request {
                repo: parent,
                path: PathBuf::from("renamed"),
                summary_only: false,
                group: Some(Group::Index),
                original_path: Some(PathBuf::from("child")),
            },
            &SubmoduleConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(
            error
                .message
                .contains("submodule renames cannot be inspected")
        );
    }

    #[test]
    fn removed_submodule_checkout_reports_it_is_unavailable() {
        if !crate::git::git_test_available() {
            return;
        }
        let (_temp, parent, child, scope) = fixture();
        let output = crate::git::process::git_command(&parent)
            .args(["rm", "-f", "--", "child"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!child.exists());
        let error = request_group(&parent, "child", &scope, Group::Index).unwrap_err();
        assert!(matches!(error.code, "invalid_request" | "repository_error"));
        assert!(error.message.contains("unavailable") || error.message.contains("not initialized"));
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
        let compact = inspect(
            &scope,
            Request {
                repo: parent.clone(),
                path: PathBuf::from("child"),
                summary_only: true,
                group: Some(Group::Working),
                original_path: None,
            },
            &SubmoduleConfig::default(),
        )
        .unwrap();
        assert_eq!(compact["status"]["file_count_lower_bound"], 1);
        assert_eq!(compact["status"]["files"], json!([]));
        assert_eq!(compact["status"]["files_truncated"], true);
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
        assert_eq!(
            request(&parent, "bad\0name", &scope).unwrap_err().code,
            "invalid_request"
        );
        assert!(
            serde_json::from_value::<Request>(json!({
                "repo": parent, "path": "child", "group": "unknown"
            }))
            .is_err()
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
