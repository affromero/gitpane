use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::{
    BridgeError,
    changes::{DirectoryIdentity, RepositoryIdentity},
    scope::Scope,
};

fn head_label(path: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut contents = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(4097)
        .read_to_string(&mut contents)
        .ok()?;
    if contents.len() > 4096 {
        return None;
    }
    let head = contents.trim_end_matches(['\r', '\n']);
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return git2::Reference::is_valid_name(&format!("refs/heads/{branch}"))
            .then(|| branch.to_owned());
    }
    if (head.len() == 40 || head.len() == 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(head[..8].to_owned());
    }
    None
}

fn registrations(repo: &git2::Repository) -> Result<Vec<(PathBuf, Option<String>)>, BridgeError> {
    let error = |error: git2::Error| BridgeError::new("repository_error", error.to_string());
    let mut paths = Vec::new();
    let names = repo.worktrees().map_err(error)?;
    for name in names.iter() {
        let name = name.map_err(error)?.ok_or_else(|| {
            BridgeError::new("repository_error", "Worktree name is not valid UTF-8")
        })?;
        let name_path = Path::new(name);
        let branch = if matches!(
            name_path.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        ) {
            head_label(&repo.commondir().join("worktrees").join(name).join("HEAD"))
        } else {
            None
        };
        paths.push((
            repo.find_worktree(name)
                .map_err(error)?
                .path()
                .components()
                .collect(),
            branch,
        ));
    }
    // A linked worktree can be the workspace anchor. Git's worktree list
    // excludes the main checkout, whose path is recorded by the common repo.
    let common = git2::Repository::open(repo.commondir()).map_err(error)?;
    if let Some(path) = common.workdir() {
        paths.push((
            path.components().collect(),
            head_label(&repo.commondir().join("HEAD")),
        ));
    }
    Ok(paths)
}

pub(in crate::bridge) fn list(
    scope: &Scope,
    repo: &git2::Repository,
) -> Result<Vec<Value>, BridgeError> {
    let mut result = Vec::new();
    for (path, branch) in registrations(repo)? {
        if !path.is_absolute() {
            continue;
        }
        let path_text = path.to_str().ok_or_else(|| {
            BridgeError::new("repository_error", "Worktree path is not valid UTF-8")
        })?;
        let canonical = match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(error) => {
                if scope.allows_missing(&path).unwrap_or(false) {
                    continue;
                }
                result.push(
                    json!({ "path": path_text, "identity": null, "branch": branch,
                    "status_error": format!("Worktree is unavailable: {error}") }),
                );
                continue;
            }
        };
        if scope.allows(&canonical) {
            continue;
        }
        match DirectoryIdentity::read(&canonical).and_then(|identity| identity.fingerprint()) {
            Ok(identity) => {
                result.push(json!({ "path": path_text, "identity": identity, "branch": branch }))
            }
            Err(error) => result.push(
                json!({ "path": path_text, "identity": null, "branch": branch,
                "status_error": format!("Worktree is unavailable: {}", error.message) }),
            ),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{Selection, head_label, list, validate};
    use crate::bridge::{changes::RepositoryIdentity, scope::Scope};
    use git2::Repository;
    use std::path::Path;

    #[test]
    fn head_labels_include_branches_and_detached_commits() {
        let temp = tempfile::tempdir().unwrap();
        let head = temp.path().join("HEAD");
        std::fs::write(&head, "ref: refs/heads/wt/model-backends\n").unwrap();
        assert_eq!(head_label(&head).as_deref(), Some("wt/model-backends"));
        std::fs::write(&head, "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(head_label(&head).as_deref(), Some("01234567"));
        std::fs::write(&head, "ref: refs/heads/invalid branch\n").unwrap();
        assert_eq!(head_label(&head), None);
        std::fs::write(&head, "a".repeat(4097)).unwrap();
        assert_eq!(head_label(&head), None);
    }

    #[test]
    fn external_worktrees_are_listed_and_revalidated_without_widening_scope() {
        if !crate::git::git_test_available() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        let main = main.canonicalize().unwrap();
        let repo = Repository::init(&main).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let sibling = temp.path().join("sibling");
        let output = crate::git::process::git_command(&main)
            .args(["worktree", "add", "-b", "topic"])
            .arg(crate::repo_id::boundary_path(&sibling).as_ref())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let sibling = sibling.canonicalize().unwrap();
        let scope = Scope::new(vec![main.clone()]).unwrap();
        let listed = list(&scope, &repo).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["branch"], "topic");
        let registered = Path::new(listed[0]["path"].as_str().unwrap());
        assert_eq!(registered.canonicalize().unwrap(), sibling);
        let target_identity = listed[0]["identity"].as_str().unwrap().to_owned();
        let identity = RepositoryIdentity::read(&main)
            .unwrap()
            .fingerprint()
            .unwrap();
        let selection = |path: &Path, identity: &str| Selection {
            repo: main.clone(),
            path: path.to_path_buf(),
            expected_repo_identity: identity.into(),
            expected_worktree_identity: target_identity.clone(),
            chain: Vec::new(),
        };
        assert_eq!(
            validate(&scope, selection(registered, &identity)).unwrap()["path"],
            crate::repo_id::boundary_path(&sibling).to_str().unwrap()
        );
        assert_eq!(
            scope.repository(&sibling).unwrap_err().code,
            "outside_workspace"
        );
        assert_eq!(
            validate(&scope, selection(registered, &"0".repeat(64)))
                .unwrap_err()
                .code,
            "stale_target"
        );
        assert_eq!(
            validate(&scope, selection(temp.path(), &identity))
                .unwrap_err()
                .code,
            "stale_target"
        );
        let linked_scope = Scope::new(vec![sibling.clone()]).unwrap();
        let linked = Repository::open(&sibling).unwrap();
        let listed_main = list(&linked_scope, &linked).unwrap();
        assert_eq!(
            listed_main[0]["branch"],
            repo.head().unwrap().shorthand().unwrap()
        );
        assert_eq!(
            Path::new(listed_main[0]["path"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            main
        );
        let saved = temp.path().join("saved-sibling");
        std::fs::rename(&sibling, &saved).unwrap();
        let unavailable = list(&scope, &repo).unwrap();
        assert_eq!(unavailable.len(), 1);
        assert!(unavailable[0]["identity"].is_null());
        assert!(
            unavailable[0]["status_error"]
                .as_str()
                .unwrap()
                .contains("unavailable")
        );
        std::fs::create_dir(&sibling).unwrap();
        assert_eq!(
            validate(&scope, selection(registered, &identity))
                .unwrap_err()
                .code,
            "stale_target"
        );
        std::fs::remove_dir(&sibling).unwrap();
        std::fs::rename(&saved, &sibling).unwrap();
        let output = crate::git::process::git_command(&main)
            .args(["worktree", "remove"])
            .arg(crate::repo_id::boundary_path(&sibling).as_ref())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            validate(&scope, selection(registered, &identity))
                .unwrap_err()
                .code,
            "stale_target"
        );
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct Selection {
    repo: PathBuf,
    path: PathBuf,
    expected_repo_identity: String,
    expected_worktree_identity: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    chain: Vec<Value>,
}

pub(in crate::bridge) fn read_scope(
    workspace: &Scope,
    selection: &Selection,
    requested_repo: &Path,
) -> Result<Scope, BridgeError> {
    let value = validate(workspace, selection.clone())?;
    let path = value["path"]
        .as_str()
        .ok_or_else(|| BridgeError::new("repository_error", "Invalid validated worktree path."))?;
    let canonical = Path::new(path).canonicalize().map_err(|_| {
        BridgeError::new("stale_target", "Worktree changed. Refresh and try again.")
    })?;
    let temporary = Scope::new(vec![canonical.clone()])?;
    let target = if selection.chain.is_empty() {
        canonical
    } else {
        let request =
            serde_json::from_value(json!({ "root": canonical, "chain": selection.chain }))
                .map_err(|error| BridgeError::new("invalid_request", error.to_string()))?;
        let proof = super::super::submodule::pin::validate(&temporary, request)?;
        proof["repos"]
            .as_array()
            .and_then(|repos| repos.last())
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| BridgeError::new("repository_error", "Invalid submodule proof."))?
    };
    if requested_repo.canonicalize().ok().as_ref() != Some(&target) {
        return Err(BridgeError::new(
            "outside_workspace",
            "Request does not identify the selected worktree.",
        ));
    }
    Ok(temporary)
}

pub(in crate::bridge) fn validate(scope: &Scope, input: Selection) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&repo)?;
    let stale = || BridgeError::new("stale_target", "Worktree changed. Refresh and try again.");
    if identity.fingerprint()? != input.expected_repo_identity || !input.path.is_absolute() {
        return Err(stale());
    }
    let repository = git2::Repository::open(&repo)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    if !registrations(&repository)?
        .iter()
        .any(|(path, _)| path == &input.path)
    {
        return Err(stale());
    }
    let target = input.path.canonicalize().map_err(|_| stale())?;
    if scope.allows(&target) {
        return Err(stale());
    }
    if DirectoryIdentity::read(&target)?.fingerprint()? != input.expected_worktree_identity {
        return Err(stale());
    }
    let target_repo = git2::Repository::open(&target).map_err(|_| stale())?;
    if target_repo
        .workdir()
        .and_then(|path: &Path| path.canonicalize().ok())
        .as_ref()
        != Some(&target)
    {
        return Err(stale());
    }
    let target_identity = RepositoryIdentity::read(&target)?;
    if !identity.same_common(&target_identity)
        || scope.repository(&input.repo)? != repo
        || RepositoryIdentity::read(&repo)? != identity
        || !registrations(&repository)?
            .iter()
            .any(|(path, _)| path == &input.path)
        || input.path.canonicalize().ok().as_ref() != Some(&target)
        || DirectoryIdentity::read(&target)?.fingerprint()? != input.expected_worktree_identity
        || RepositoryIdentity::read(&target)? != target_identity
    {
        return Err(stale());
    }
    Ok(
        json!({ "path": crate::repo_id::boundary_path(&target).to_str().ok_or_else(|| BridgeError::new("repository_error", "Worktree path is not valid UTF-8"))? }),
    )
}
