use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{
    BridgeError,
    changes::{DirectoryIdentity, RepositoryIdentity},
    scope::Scope,
};

fn registrations(repo: &git2::Repository) -> Result<Vec<PathBuf>, BridgeError> {
    let error = |error: git2::Error| BridgeError::new("repository_error", error.to_string());
    let mut paths = Vec::new();
    let names = repo.worktrees().map_err(error)?;
    for name in names.iter() {
        let name = name.map_err(error)?.ok_or_else(|| {
            BridgeError::new("repository_error", "Worktree name is not valid UTF-8")
        })?;
        paths.push(
            repo.find_worktree(name)
                .map_err(error)?
                .path()
                .components()
                .collect(),
        );
    }
    // A linked worktree can be the workspace anchor. Git's worktree list
    // excludes the main checkout, whose path is recorded by the common repo.
    let common = git2::Repository::open(repo.commondir()).map_err(error)?;
    if let Some(path) = common.workdir() {
        paths.push(path.components().collect());
    }
    Ok(paths)
}

pub(in crate::bridge) fn list(
    scope: &Scope,
    repo: &git2::Repository,
) -> Result<Vec<Value>, BridgeError> {
    let mut result = Vec::new();
    for path in registrations(repo)? {
        if !path.is_absolute() {
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(BridgeError::new("repository_error", error.to_string())),
        };
        if scope.allows(&canonical) {
            continue;
        }
        let path = path.to_str().ok_or_else(|| {
            BridgeError::new("repository_error", "Worktree path is not valid UTF-8")
        })?;
        result.push(json!({ "path": path, "identity": DirectoryIdentity::read(&canonical)?.fingerprint()? }));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{Selection, list, validate};
    use crate::bridge::{changes::RepositoryIdentity, scope::Scope};
    use git2::Repository;
    use std::path::Path;

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
            Path::new(listed_main[0]["path"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            main
        );
        let saved = temp.path().join("saved-sibling");
        std::fs::rename(&sibling, &saved).unwrap();
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct Selection {
    repo: PathBuf,
    path: PathBuf,
    expected_repo_identity: String,
    expected_worktree_identity: String,
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
        .any(|path| path == &input.path)
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
            .any(|path| path == &input.path)
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
