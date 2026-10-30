use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{WorktreeConfig, worktree_path};

use super::{BridgeError, mutation, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorktreePlan {
    repo: PathBuf,
    branch: String,
}

pub(super) fn plan(
    scope: &Scope,
    input: WorktreePlan,
    config: &WorktreeConfig,
) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    mutation::validate_worktree_branch(&input.branch)?;
    let suggested = worktree_path(config, &repo, &input.branch);
    // Native Gitpane runs Git with the selected repository as its working
    // directory, so a relative configured directory is resolved from it.
    let suggested = if suggested.is_absolute() {
        suggested
    } else {
        repo.join(suggested)
    };
    // Resolve an existing parent so a valid relative configuration such as
    // `../worktrees` yields an absolute destination without traversal.
    let suggested = match (suggested.parent(), suggested.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => suggested,
        },
        _ => suggested,
    };
    let problem = mutation::validate_worktree_destination(scope, &suggested)
        .err()
        .map(|error| format!("Suggested worktree path is unavailable: {}", error.message));
    let path = suggested.to_str().ok_or_else(|| {
        BridgeError::new(
            "repository_error",
            "worktree destination is not valid UTF-8",
        )
    })?;
    scope.repository(&repo)?;
    Ok(json!({ "path": path, "problem": problem }))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use git2::Repository;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::bridge::serve;
    use crate::config::{GraphConfig, SubmoduleConfig, WorktreeConfig};

    fn exchange(workspace: &Path, requests: &[Value], config: &WorktreeConfig) -> Vec<Value> {
        let input = std::iter::once(json!({"id": 1, "method": "initialize", "params": {
            "version": 1, "workspace_roots": [workspace]
        }}))
        .chain(requests.iter().cloned())
        .map(|request| format!("{request}\n"))
        .collect::<String>();
        let mut output = Vec::new();
        serve(
            &mut Cursor::new(input),
            &mut output,
            &SubmoduleConfig::default(),
            &GraphConfig::default(),
            config,
            None,
            false,
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn request(repo: &Path, branch: &str) -> Value {
        json!({"id": 2, "method": "worktree_plan", "params": {"repo": repo, "branch": branch}})
    }

    #[test]
    fn suggested_paths_follow_gitpane_configuration_and_workspace_scope() {
        let temporary = TempDir::new().unwrap();
        let workspace = temporary.path().join("workspace");
        let repo = workspace.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Repository::init(&repo).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let repo = repo.canonicalize().unwrap();
        let request = request(&repo, "feature/search");

        let result = exchange(
            &workspace,
            std::slice::from_ref(&request),
            &WorktreeConfig::default(),
        );
        assert_eq!(
            result[1]["result"]["path"],
            workspace.join("repo-feature-search").to_str().unwrap()
        );
        assert!(result[1]["result"]["problem"].is_null());

        let result = exchange(
            &repo,
            std::slice::from_ref(&request),
            &WorktreeConfig::default(),
        );
        assert_eq!(
            result[1]["result"]["path"],
            workspace.join("repo-feature-search").to_str().unwrap()
        );
        assert!(
            result[1]["result"]["problem"]
                .as_str()
                .unwrap()
                .contains("outside the workspace")
        );

        let worktrees = workspace.join("worktrees");
        std::fs::create_dir(&worktrees).unwrap();
        let config = WorktreeConfig {
            dir: Some(PathBuf::from("../worktrees")),
        };
        let result = exchange(&workspace, std::slice::from_ref(&request), &config);
        assert_eq!(
            result[1]["result"]["path"],
            worktrees.join("repo-feature-search").to_str().unwrap()
        );
        assert!(result[1]["result"]["problem"].is_null());

        let config = WorktreeConfig {
            dir: Some(workspace.join("missing")),
        };
        let result = exchange(&workspace, std::slice::from_ref(&request), &config);
        assert_eq!(
            result[1]["result"]["path"],
            workspace
                .join("missing/repo-feature-search")
                .to_str()
                .unwrap()
        );
        assert!(result[1]["result"]["problem"].is_string());

        std::fs::create_dir(worktrees.join("repo-feature-search")).unwrap();
        let config = WorktreeConfig {
            dir: Some(worktrees),
        };
        let result = exchange(&workspace, &[request], &config);
        assert!(
            result[1]["result"]["problem"]
                .as_str()
                .unwrap()
                .contains("already exists")
        );
    }

    #[test]
    fn rejects_invalid_branches_and_repositories_outside_workspace() {
        let temporary = TempDir::new().unwrap();
        let workspace = temporary.path().join("workspace");
        let repo = workspace.join("repo");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Repository::init(&repo).unwrap();
        Repository::init(&outside).unwrap();
        let requests = [
            request(&repo, "-invalid"),
            request(&repo, "a..b"),
            request(&outside, "topic"),
        ];
        let result = exchange(&workspace, &requests, &WorktreeConfig::default());
        assert_eq!(result[1]["error"]["code"], "invalid_request");
        assert_eq!(result[2]["error"]["code"], "invalid_request");
        assert_eq!(result[3]["error"]["code"], "outside_workspace");
    }

    #[cfg(unix)]
    #[test]
    fn configured_symlink_cannot_hide_an_outside_destination() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().unwrap();
        let workspace = temporary.path().join("workspace");
        let repo = workspace.join("repo");
        let outside = temporary.path().join("outside");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Repository::init(&repo).unwrap();
        let link = workspace.join("worktrees");
        symlink(&outside, &link).unwrap();
        let config = WorktreeConfig { dir: Some(link) };
        let result = exchange(&workspace, &[request(&repo, "topic")], &config);
        assert_eq!(
            result[1]["result"]["path"],
            outside
                .canonicalize()
                .unwrap()
                .join("repo-topic")
                .to_str()
                .unwrap()
        );
        assert!(
            result[1]["result"]["problem"]
                .as_str()
                .unwrap()
                .contains("outside the workspace")
        );
    }
}
