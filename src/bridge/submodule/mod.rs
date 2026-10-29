use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::SubmoduleConfig;
use crate::git::commit_files::list_revision_files;

use super::{BridgeError, changes::RepositoryIdentity, commit_file_json, scope::Scope, status};

pub(super) mod actions;
pub(super) mod chain;
pub(super) mod pin;
pub(super) mod pointer;

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

fn head_filemode(
    repo: &git2::Repository,
    path: &std::path::Path,
) -> Result<Option<u32>, BridgeError> {
    Ok(match repo.head() {
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
    })
}

fn ensure_no_type_change(
    repo: &git2::Repository,
    path: &std::path::Path,
) -> Result<(), BridgeError> {
    let head_mode = head_filemode(repo, path)?;
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

#[derive(PartialEq, Eq)]
struct LinkSnapshot {
    parent_path: PathBuf,
    child_path: PathBuf,
    child: PathBuf,
    parent_identity: RepositoryIdentity,
    child_identity: RepositoryIdentity,
    parent_head: Option<git2::Oid>,
    index: Option<git2::Oid>,
    child_head: git2::Oid,
    base: Option<git2::Oid>,
    head: Option<git2::Oid>,
}

fn valid_submodule_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

struct RegisteredLink {
    parent_path: PathBuf,
    child_path: PathBuf,
    child: PathBuf,
    parent_identity: RepositoryIdentity,
    child_identity: RepositoryIdentity,
    parent_head: Option<git2::Oid>,
    index: Option<git2::Oid>,
}

fn registered_link(
    scope: &Scope,
    repo: &Path,
    path: &Path,
    pin: bool,
) -> Result<RegisteredLink, BridgeError> {
    if !valid_submodule_path(path) {
        return Err(BridgeError::new(
            "invalid_request",
            "submodule path must be relative",
        ));
    }
    let parent_path = scope.repository(repo)?;
    let parent_identity = RepositoryIdentity::read(&parent_path)?;
    scope.validate_modules_file(&parent_path)?;
    let parent = git2::Repository::open(&parent_path).map_err(error)?;
    let modules = parent.submodules().map_err(error)?;
    let sub = modules
        .iter()
        .find(|sub| sub.path() == path)
        .ok_or_else(|| {
            BridgeError::new(
                "invalid_request",
                "Submodule is no longer registered or unavailable.",
            )
        })?;
    if pin {
        require_pin_registration(&parent_path, sub, path)?;
        require_pin_path(&parent_path, path)?;
    }
    let child_path = parent_path.join(path);
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
    if pin && !child.starts_with(&parent_path) {
        return Err(BridgeError::new(
            "outside_workspace",
            "submodule resolves outside its parent repository",
        ));
    }
    let child_identity = RepositoryIdentity::read(&child)?;
    Ok(RegisteredLink {
        parent_path,
        child_path,
        child,
        parent_identity,
        child_identity,
        parent_head: sub.head_id(),
        index: sub.index_id(),
    })
}

fn require_pin_registration(
    parent_path: &Path,
    sub: &git2::Submodule<'_>,
    path: &Path,
) -> Result<(), BridgeError> {
    let no_registration = || {
        BridgeError::new(
            "invalid_request",
            "Submodule is no longer registered or unavailable.",
        )
    };
    let modules_path = parent_path.join(".gitmodules");
    match modules_path.metadata() {
        Ok(_) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            return Err(no_registration());
        }
        Err(failure) => return Err(error(failure)),
    }
    let config = git2::Config::open(&modules_path).map_err(error)?;
    let key = format!("submodule.{}.path", sub.name().map_err(error)?);
    let declared = config.get_string(&key).map_err(|failure| {
        if failure.code() == git2::ErrorCode::NotFound {
            no_registration()
        } else {
            error(failure)
        }
    })?;
    if Path::new(&declared) != path {
        return Err(no_registration());
    }
    Ok(())
}

fn require_pin_path(parent_path: &Path, path: &Path) -> Result<(), BridgeError> {
    let mut component_path = parent_path.to_path_buf();
    for component in path.components() {
        component_path.push(component.as_os_str());
        match component_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BridgeError::new(
                    "outside_workspace",
                    "submodule path must not contain a symlink",
                ));
            }
            Ok(_) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => break,
            Err(failure) => return Err(error(failure)),
        }
    }
    Ok(())
}

fn read_link(scope: &Scope, input: &Request) -> Result<LinkSnapshot, BridgeError> {
    if !valid_submodule_path(&input.path) {
        return Err(BridgeError::new(
            "invalid_request",
            "submodule path must be relative",
        ));
    }
    let parent_path = scope.repository(&input.repo)?;
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
    let parent = git2::Repository::open(&parent_path).map_err(error)?;
    ensure_no_conflict(&parent, &input.path)?;
    ensure_supported_type(&parent, &input.path, input.group)?;
    let registered = registered_link(scope, &parent_path, &input.path, false)?;
    let child_repo = git2::Repository::open(&registered.child).map_err(error)?;
    let child_head = child_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(error)?
        .id();
    let parent_head = registered.parent_head;
    let index = registered.index;
    let (base, head) = match input.group {
        Some(Group::Index) => (parent_head, index),
        Some(Group::Working | Group::Attention) => (index, Some(child_head)),
        None => (parent_head, Some(child_head)),
        Some(Group::Merge) => unreachable!(),
    };
    Ok(LinkSnapshot {
        parent_path: registered.parent_path,
        child_path: registered.child_path,
        child: registered.child,
        parent_identity: registered.parent_identity,
        child_identity: registered.child_identity,
        parent_head,
        index,
        child_head,
        base,
        head,
    })
}

pub(super) fn inspect(
    scope: &Scope,
    input: Request,
    sub_cfg: &SubmoduleConfig,
) -> Result<Value, BridgeError> {
    let parent = scope.repository(&input.repo)?;
    scope.validate_submodules(&parent)?;
    let initial = read_link(scope, &input)?;
    if initial.parent_path != parent {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    let files = if initial.base == initial.head {
        Vec::new()
    } else {
        list_revision_files(&initial.child, initial.base, initial.head).map_err(error)?
    };
    let child_status = status::query(
        &initial.child,
        scope,
        sub_cfg,
        true,
        input.summary_only,
        &[],
    )?;
    scope.validate_submodules(&parent)?;
    let current = read_link(scope, &input).map_err(|failure| {
        if failure.code == "invalid_request" {
            BridgeError::new(
                "stale_target",
                "Submodule changed; refresh before inspecting",
            )
        } else {
            failure
        }
    })?;
    if current != initial {
        return Err(BridgeError::new(
            "stale_target",
            "Submodule changed; refresh before inspecting",
        ));
    }
    let child_text = initial
        .child
        .to_str()
        .ok_or_else(|| error("submodule path is not valid UTF-8"))?;
    Ok(json!({
        "repo": child_text,
        "base_oid": initial.base.map(|id| id.to_string()),
        "head_oid": initial.head.map(|id| id.to_string()),
        "child_head_oid": initial.child_head.to_string(),
        "parent_identity": initial.parent_identity.fingerprint()?,
        "child_identity": initial.child_identity.fingerprint()?,
        "status": child_status,
        "pointer_files": files.into_iter().map(commit_file_json).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::path::Path;

    pub(super) fn commit_file(repo: &Repository, name: &str, text: &str) -> git2::Oid {
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

    pub(super) fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Scope) {
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
    fn staged_gitlink_and_registration_removal_keeps_initialized_child_inspectable() {
        let (_temp, parent, child, scope) = fixture();
        let repository = Repository::open(&parent).unwrap();
        let mut index = repository.index().unwrap();
        index.remove_path(Path::new("child")).unwrap();
        std::fs::write(parent.join(".gitmodules"), "").unwrap();
        index.add_path(Path::new(".gitmodules")).unwrap();
        index.write().unwrap();
        assert!(child.join(".git").exists());
        let deleted = request_group(&parent, "child", &scope, Group::Index).unwrap();
        assert!(deleted["base_oid"].is_string());
        assert!(deleted["head_oid"].is_null());
        assert!(
            deleted["pointer_files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["status"] == "D")
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspection_preserves_registered_in_scope_symlink() {
        use std::os::unix::fs::symlink;
        let (temp, parent, child, scope) = fixture();
        let moved = temp.path().join("workspace/moved-child");
        let gitdir = Repository::open(&child)
            .unwrap()
            .path()
            .canonicalize()
            .unwrap();
        std::fs::rename(&child, &moved).unwrap();
        std::fs::write(
            moved.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        symlink(&moved, &child).unwrap();
        let result = request_group(&parent, "child", &scope, Group::Working).unwrap();
        assert_eq!(
            Path::new(result["repo"].as_str().unwrap()),
            moved.canonicalize().unwrap()
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
