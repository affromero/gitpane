use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::git::{file_ops, process, worktree_ops};

use super::{BridgeError, changes::RepositoryIdentity, file_identity, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileAction {
    repo: PathBuf,
    path: PathBuf,
    operation: String,
    #[serde(default)]
    expected_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepoSync {
    repo: PathBuf,
    operation: String,
    expected_branch: Option<String>,
    expected_head: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateWorktree {
    repo: PathBuf,
    branch: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveWorktree {
    repo: PathBuf,
    path: PathBuf,
    #[serde(default)]
    expected_repo_identity: Option<String>,
    #[serde(default)]
    expected_worktree_identity: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreviewWorktree {
    repo: PathBuf,
    path: PathBuf,
}

fn repo_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn run(repo: &Path, args: &[String]) -> Result<Value, BridgeError> {
    let output = process::run_git_op_capturing(repo, args).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "git_error"
        } else {
            "outcome_unknown"
        };
        BridgeError::new(
            code,
            format!("Git operation could not be completed: {error}"),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.trim().chars().take(2048).collect();
        return Err(BridgeError::new(
            "git_error",
            format!("Git operation failed ({}): {detail}", output.status),
        ));
    }
    Ok(json!({}))
}

fn relative_path(path: &Path) -> Result<(), BridgeError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let ends_in_separator = bytes
        .last()
        .is_some_and(|byte| *byte == b'/' || (cfg!(windows) && *byte == b'\\'));
    if bytes.is_empty()
        || ends_in_separator
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(BridgeError::new(
            "invalid_request",
            "file path must be a nonempty relative path without traversal",
        ));
    }
    Ok(())
}

pub(super) fn verify_sync_head(
    repo_path: &Path,
    expected_branch: Option<&str>,
    expected_head: &str,
) -> Result<String, BridgeError> {
    let repo = git2::Repository::open(repo_path).map_err(repo_error)?;
    let head = repo.head().map_err(|error| {
        if error.code() == git2::ErrorCode::UnbornBranch {
            BridgeError::new(
                "stale_target",
                "Cannot sync a repository without a HEAD commit",
            )
        } else {
            repo_error(error)
        }
    })?;
    let oid = head
        .target()
        .or_else(|| head.peel_to_commit().ok().map(|commit| commit.id()))
        .ok_or_else(|| repo_error("HEAD does not identify a commit"))?;
    let detached = repo.head_detached().map_err(repo_error)?;
    let branch = if detached {
        None
    } else {
        Some(head.shorthand().map_err(repo_error)?)
    };
    if branch != expected_branch || oid.to_string() != expected_head {
        return Err(BridgeError::new(
            "stale_target",
            "Branch or HEAD changed; refresh before syncing",
        ));
    }
    Ok(branch.unwrap_or("HEAD").to_owned())
}

pub(super) fn repo_sync(scope: &Scope, input: RepoSync) -> Result<Value, BridgeError> {
    let repo_path = scope.repository(&input.repo)?;
    let branch = verify_sync_head(
        &repo_path,
        input.expected_branch.as_deref(),
        &input.expected_head,
    )?;
    let (push, action): (bool, &[&str]) = match input.operation.as_str() {
        "push" => (true, &["push"]),
        "pull" => (false, &["pull", "--no-recurse-submodules"]),
        "pull_rebase" => (false, &["pull", "--rebase", "--no-recurse-submodules"]),
        _ => {
            return Err(BridgeError::new(
                "invalid_request",
                "unknown repository sync action",
            ));
        }
    };
    let mut args: Vec<String> = [
        "-c",
        "submodule.recurse=false",
        "-c",
        "fetch.recurseSubmodules=false",
        "-c",
        "push.recurseSubmodules=no",
    ]
    .into_iter()
    .chain(action.iter().copied())
    .map(str::to_owned)
    .collect();
    args = crate::git::sync_arguments(&repo_path, &branch, push, args);
    scope.repository(&repo_path)?;
    verify_sync_head(
        &repo_path,
        input.expected_branch.as_deref(),
        &input.expected_head,
    )?;
    run(&repo_path, &args)
}

pub(super) fn validate_file_path(
    scope: &Scope,
    repo: &Path,
    path: &Path,
) -> Result<(), BridgeError> {
    relative_path(path)?;
    let full = repo.join(path);
    let parent = full
        .parent()
        .ok_or_else(|| BridgeError::new("invalid_request", "file path has no parent"))?;
    if !scope.allows_missing(parent)? {
        return Err(BridgeError::new(
            "outside_workspace",
            "file path is outside the workspace",
        ));
    }
    // Git operates on the leaf entry itself, including symlinks. Only the
    // directory chain may be resolved: following the leaf would reject valid
    // links to external or missing targets.
    let mut current = parent;
    loop {
        match current.canonicalize() {
            Ok(canonical) if canonical.starts_with(repo) => return Ok(()),
            Ok(_) => {
                return Err(BridgeError::new(
                    "outside_workspace",
                    "file path resolves outside the repository",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current.parent().ok_or_else(|| repo_error(error))?;
            }
            Err(error) => return Err(repo_error(error)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkingSymlink {
    repo: PathBuf,
    path: PathBuf,
}

pub(super) fn working_symlink(scope: &Scope, input: WorkingSymlink) -> Result<Value, BridgeError> {
    let repo_path = scope.repository(&input.repo)?;
    validate_file_path(scope, &repo_path, &input.path)?;
    let repo = git2::Repository::open(&repo_path).map_err(repo_error)?;
    let status = repo.status_file(&input.path).map_err(repo_error)?;
    let working = git2::Status::WT_NEW
        | git2::Status::WT_MODIFIED
        | git2::Status::WT_TYPECHANGE
        | git2::Status::WT_RENAMED;
    if !status.intersects(working) {
        return Err(BridgeError::new(
            "stale_target",
            "The symlink change is no longer available.",
        ));
    }
    let target = file_identity::link_target(&repo_path.join(&input.path))?;
    if scope.repository(&input.repo)? != repo_path
        || repo.status_file(&input.path).map_err(repo_error)? != status
    {
        return Err(BridgeError::new(
            "stale_target",
            "The symlink change is no longer available.",
        ));
    }
    validate_file_path(scope, &repo_path, &input.path)?;
    Ok(json!({ "target": target }))
}

fn prepare_file_action(
    scope: &Scope,
    input: &FileAction,
) -> Result<(PathBuf, file_ops::FileOperation), BridgeError> {
    let repo_path = scope.repository(&input.repo)?;
    validate_file_path(scope, &repo_path, &input.path)?;
    scope.validate_submodules(&repo_path)?;
    let repo = git2::Repository::open(&repo_path).map_err(repo_error)?;
    for submodule in repo.submodules().map_err(repo_error)? {
        let sub = submodule.path();
        if input.path.starts_with(sub) || sub.starts_with(&input.path) {
            return Err(BridgeError::new(
                "invalid_request",
                "file actions are unavailable for submodules",
            ));
        }
    }
    let status = repo.status_file(&input.path).map_err(repo_error)?;
    let staged = status.intersects(
        git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED
            | git2::Status::INDEX_RENAMED
            | git2::Status::INDEX_TYPECHANGE,
    );
    let unstaged = status.intersects(
        git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::WT_DELETED
            | git2::Status::WT_TYPECHANGE
            | git2::Status::WT_RENAMED
            | git2::Status::CONFLICTED,
    );
    let operation = match input.operation.as_str() {
        "stage" if unstaged => file_ops::FileOperation::Stage,
        "unstage" if staged => file_ops::FileOperation::Unstage,
        "discard" if staged || (unstaged && !status.is_wt_new()) => {
            file_ops::FileOperation::Discard
        }
        "delete_untracked" if status.is_wt_new() && !staged => {
            file_ops::FileOperation::DeleteUntracked
        }
        "stage" | "unstage" | "discard" | "delete_untracked" => {
            return Err(BridgeError::new(
                "invalid_request",
                "file status changed; refresh before applying this action",
            ));
        }
        _ => return Err(BridgeError::new("invalid_request", "unknown file action")),
    };
    Ok((repo_path, operation))
}

pub(super) fn file_action_preview(scope: &Scope, input: FileAction) -> Result<Value, BridgeError> {
    if input.expected_token.is_some() {
        return Err(BridgeError::new(
            "invalid_request",
            "preview does not accept an identity token",
        ));
    }
    let (repo_path, operation) = prepare_file_action(scope, &input)?;
    if !matches!(
        operation,
        file_ops::FileOperation::Discard | file_ops::FileOperation::DeleteUntracked
    ) {
        return Err(BridgeError::new(
            "invalid_request",
            "only destructive file actions require confirmation",
        ));
    }
    let token = file_identity::token(&repo_path, &input.path, operation, |path| {
        validate_file_path(scope, &repo_path, path)
    })?;
    scope.repository(&repo_path)?;
    Ok(json!({ "token": token }))
}

pub(super) fn file_action(scope: &Scope, input: FileAction) -> Result<Value, BridgeError> {
    let (repo_path, operation) = prepare_file_action(scope, &input)?;
    scope.repository(&repo_path)?;
    let args = if matches!(
        operation,
        file_ops::FileOperation::Discard | file_ops::FileOperation::DeleteUntracked
    ) {
        let expected = input
            .expected_token
            .as_deref()
            .filter(|token| token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| {
                BridgeError::new(
                    "invalid_request",
                    "destructive file action requires confirmation",
                )
            })?;
        let (actual, args) = file_identity::plan(&repo_path, &input.path, operation, |path| {
            validate_file_path(scope, &repo_path, path)
        })?;
        if actual != expected {
            return Err(BridgeError::new(
                "file_changed",
                "File or index changed; review the change before discarding",
            ));
        }
        args
    } else {
        if input.expected_token.is_some() {
            return Err(BridgeError::new(
                "invalid_request",
                "identity token is only valid for destructive actions",
            ));
        }
        let args = file_ops::arguments(&repo_path, &input.path, operation).map_err(repo_error)?;
        for path in args.iter().skip_while(|arg| *arg != "--").skip(1) {
            validate_file_path(scope, &repo_path, Path::new(path))?;
        }
        args
    };
    run(&repo_path, &args)
}

fn absolute_path(path: &Path) -> Result<(), BridgeError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(BridgeError::new(
            "invalid_request",
            "worktree path must be absolute without traversal",
        ));
    }
    Ok(())
}

pub(super) fn validate_worktree_branch(branch: &str) -> Result<(), BridgeError> {
    if branch.starts_with('-') || !git2::Branch::name_is_valid(branch).map_err(repo_error)? {
        return Err(BridgeError::new("invalid_request", "invalid branch name"));
    }
    Ok(())
}

pub(super) fn validate_worktree_destination(
    scope: &Scope,
    path: &Path,
) -> Result<PathBuf, BridgeError> {
    absolute_path(path)?;
    match path.symlink_metadata() {
        Ok(_) => {
            return Err(BridgeError::new(
                "invalid_request",
                "worktree destination already exists",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(repo_error(error)),
    }
    let parent = path
        .parent()
        .ok_or_else(|| BridgeError::new("invalid_request", "worktree destination has no parent"))?;
    let canonical_parent = parent.canonicalize().map_err(repo_error)?;
    if !canonical_parent.is_dir() || !scope.allows(&canonical_parent) {
        return Err(BridgeError::new(
            "outside_workspace",
            "worktree destination is outside the workspace",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| BridgeError::new("invalid_request", "worktree destination has no name"))?;
    Ok(canonical_parent.join(name))
}

pub(super) fn worktree_create(scope: &Scope, input: CreateWorktree) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    absolute_path(&input.path)?;
    validate_worktree_branch(&input.branch)?;
    let target = validate_worktree_destination(scope, &input.path)?;
    scope.repository(&repo)?;
    let mut args = vec!["-c".to_owned(), "submodule.recurse=false".to_owned()];
    args.extend(worktree_ops::create_arguments(&input.branch, &target));
    run(&repo, &args)
}

#[derive(PartialEq, Eq)]
struct WorktreeSelection {
    repo: PathBuf,
    target: PathBuf,
    common_dir: PathBuf,
    repo_identity: RepositoryIdentity,
    worktree_identity: RepositoryIdentity,
}

fn registered_worktree(repo: &git2::Repository, target: &Path) -> Result<bool, BridgeError> {
    let names = repo.worktrees().map_err(repo_error)?;
    for i in 0..names.len() {
        let name = names
            .get(i)
            .map_err(repo_error)?
            .ok_or_else(|| repo_error("worktree name is not valid UTF-8"))?;
        let worktree = repo.find_worktree(name).map_err(repo_error)?;
        if worktree
            .path()
            .canonicalize()
            .is_ok_and(|path| path == target)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn is_registered_linked_worktree(
    repo: &git2::Repository,
    path: &Path,
) -> Result<bool, BridgeError> {
    if !repo.is_worktree() {
        return Ok(false);
    }
    if !registered_worktree(repo, path)? {
        return Err(stale_worktree());
    }
    Ok(true)
}

fn stale_worktree() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Worktree changed; refresh before removing it.",
    )
}

fn worktree_selection(
    scope: &Scope,
    repo: &Path,
    path: &Path,
    expected: Option<(&str, &str)>,
) -> Result<WorktreeSelection, BridgeError> {
    let repo_path = scope.repository(repo)?;
    absolute_path(path)?;
    let target = scope.repository(path)?;
    scope.validate_worktree_removal(&target)?;
    let repo_identity = RepositoryIdentity::read(&repo_path)?;
    let worktree_identity = RepositoryIdentity::read(&target)?;
    if let Some((expected_repo, expected_worktree)) = expected
        && (repo_identity.fingerprint()? != expected_repo
            || worktree_identity.fingerprint()? != expected_worktree)
    {
        return Err(stale_worktree());
    }
    if !repo_identity.same_common(&worktree_identity) {
        return Err(if expected.is_some() {
            stale_worktree()
        } else {
            BridgeError::new(
                "invalid_request",
                "Worktree does not share this repository's Git directory.",
            )
        });
    }
    let repo = git2::Repository::open(&repo_path).map_err(repo_error)?;
    let common_dir = repo.commondir().canonicalize().map_err(repo_error)?;
    if target == repo_path && (!repo.is_worktree() || common_dir.starts_with(&target)) {
        return Err(BridgeError::new(
            "invalid_request",
            "cannot remove this repository as a linked worktree",
        ));
    }
    if !registered_worktree(&repo, &target)? {
        return Err(if expected.is_some() {
            stale_worktree()
        } else {
            BridgeError::new(
                "invalid_request",
                "worktree is not registered with this repository",
            )
        });
    }
    Ok(WorktreeSelection {
        repo: repo_path,
        target,
        common_dir,
        repo_identity,
        worktree_identity,
    })
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn worktree_remove_preview(
    scope: &Scope,
    input: PreviewWorktree,
) -> Result<Value, BridgeError> {
    let initial = worktree_selection(scope, &input.repo, &input.path, None)?;
    let current = worktree_selection(scope, &input.repo, &input.path, None)?;
    if initial != current {
        return Err(stale_worktree());
    }
    Ok(json!({
        "repo_identity": initial.repo_identity.fingerprint()?,
        "worktree_identity": initial.worktree_identity.fingerprint()?,
        "self_removal": initial.repo == initial.target,
    }))
}

pub(super) fn worktree_remove(scope: &Scope, input: RemoveWorktree) -> Result<Value, BridgeError> {
    let expected = match (
        &input.expected_repo_identity,
        &input.expected_worktree_identity,
    ) {
        (None, None) => None,
        (Some(repo), Some(worktree)) if valid_identity(repo) && valid_identity(worktree) => {
            Some((repo.as_str(), worktree.as_str()))
        }
        _ => {
            return Err(BridgeError::new(
                "invalid_request",
                "Both valid worktree identity tokens are required.",
            ));
        }
    };
    let initial = worktree_selection(scope, &input.repo, &input.path, expected)?;
    if initial.repo == initial.target && expected.is_none() {
        return Err(BridgeError::new(
            "invalid_request",
            "Worktree identity tokens are required for self-removal.",
        ));
    }
    let current = worktree_selection(scope, &input.repo, &input.path, expected)?;
    if initial != current {
        return Err(stale_worktree());
    }
    if current.repo == current.target {
        let common = current
            .common_dir
            .to_str()
            .ok_or_else(|| repo_error("Git common directory path is not valid UTF-8"))?;
        let mut args = vec![format!("--git-dir={common}")];
        args.extend(worktree_ops::remove_arguments(&current.target));
        run(&current.common_dir, &args)
    } else {
        run(
            &current.repo,
            &worktree_ops::remove_arguments(&current.target),
        )
    }
}
