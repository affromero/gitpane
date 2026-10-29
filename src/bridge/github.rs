use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::git::github::{self, CheckState, GhItem};

use super::{BridgeError, scope::Scope};

const MAX_NUMBER: u64 = 9_007_199_254_740_991;
const MAX_RESPONSE: usize = 900 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListRequest {
    repo: PathBuf,
    state: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DetailRequest {
    repo: PathBuf,
    kind: String,
    number: u64,
}

fn target(scope: &Scope, repo: &Path) -> Result<(PathBuf, String, String), BridgeError> {
    let repo = scope.repository(repo)?;
    let (owner, name) = github::github_owner_repo(&repo).ok_or_else(|| {
        BridgeError::new(
            "not_github",
            "repository has no supported github.com remote",
        )
    })?;
    if !github::safe_repository_pair(&owner, &name) {
        return Err(BridgeError::new(
            "invalid_request",
            "GitHub remote has an invalid owner or repository name",
        ));
    }
    Ok((repo, owner, name))
}

fn revalidate(scope: &Scope, repo: &Path, owner: &str, name: &str) -> Result<(), BridgeError> {
    let current = scope.repository(repo)?;
    if current != repo
        || github::github_owner_repo(repo).as_ref() != Some(&(owner.to_owned(), name.to_owned()))
    {
        return Err(BridgeError::new(
            "repository_changed",
            "repository or GitHub remote changed; refresh and try again",
        ));
    }
    Ok(())
}

fn bounded(value: Value) -> Result<Value, BridgeError> {
    let size = serde_json::to_vec(&value)
        .map_err(|err| BridgeError::new("github_error", err.to_string()))?
        .len();
    if size > MAX_RESPONSE {
        return Err(BridgeError::new(
            "response_too_large",
            "GitHub response exceeds the size limit",
        ));
    }
    Ok(value)
}

fn item(item: GhItem) -> Result<Value, BridgeError> {
    if item.number == 0 || item.number > MAX_NUMBER {
        return Err(BridgeError::new(
            "github_error",
            "GitHub returned an invalid item number",
        ));
    }
    let checks = match item.checks {
        Some(CheckState::Success) => Some("success"),
        Some(CheckState::Failure) => Some("failure"),
        Some(CheckState::Pending) => Some("pending"),
        None => None,
    };
    Ok(
        json!({"number": item.number, "title": item.title, "state": item.state,
        "draft": item.is_draft, "author": item.author, "updated_at": item.updated_at, "checks": checks}),
    )
}

pub(super) fn list(scope: &Scope, input: ListRequest) -> Result<Value, BridgeError> {
    if !matches!(input.state.as_str(), "open" | "closed" | "all") {
        return Err(BridgeError::new(
            "invalid_request",
            "invalid GitHub state filter",
        ));
    }
    let (repo, owner, name) = target(scope, &input.repo)?;
    let (issues, prs) = github::fetch_partial(&owner, &name, &input.state);
    revalidate(scope, &repo, &owner, &name)?;
    let (issues, issues_error) = match issues {
        Ok(items) => (
            items.into_iter().map(item).collect::<Result<Vec<_>, _>>()?,
            None,
        ),
        Err(error) => (Vec::new(), Some(error)),
    };
    let (prs, prs_error) = match prs {
        Ok(items) => (
            items.into_iter().map(item).collect::<Result<Vec<_>, _>>()?,
            None,
        ),
        Err(error) => (Vec::new(), Some(error)),
    };
    bounded(
        json!({"owner": owner, "repo": name, "issues": issues, "prs": prs,
        "issues_error": issues_error, "prs_error": prs_error, "limit": 50}),
    )
}

pub(super) fn detail(scope: &Scope, input: DetailRequest) -> Result<Value, BridgeError> {
    if !matches!(input.kind.as_str(), "issue" | "pr")
        || input.number == 0
        || input.number > MAX_NUMBER
    {
        return Err(BridgeError::new("invalid_request", "invalid GitHub item"));
    }
    let (repo, owner, name) = target(scope, &input.repo)?;
    let kind = if input.kind == "pr" { "pull" } else { "issues" };
    let url = format!("https://github.com/{owner}/{name}/{kind}/{}", input.number);
    let result = github::fetch_detail_partial(&url, input.kind == "pr")
        .map_err(|error| BridgeError::new("github_error", error));
    revalidate(scope, &repo, &owner, &name)?;
    let (detail, diff_error) = result?;
    if detail.number != input.number {
        return Err(BridgeError::new(
            "github_error",
            "GitHub returned a different item",
        ));
    }
    bounded(
        json!({"owner": owner, "repo": name, "kind": input.kind, "number": detail.number,
        "title": detail.title, "author": detail.author, "body": detail.body,
        "comments": detail.comments.into_iter().map(|comment| json!({"author": comment.author,
            "body": comment.body, "created_at": comment.created_at})).collect::<Vec<_>>(),
        "diff": detail.diff, "diff_error": diff_error}),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn scoped_fake_gh_reports_partial_errors_and_uses_derived_item_url() {
        if let Some(root) = std::env::var_os("GITPANE_TEST_GITHUB_ROOT") {
            let root = PathBuf::from(root);
            let repo = root.join("workspace/repo");
            let scope = Scope::new(vec![root.join("workspace")]).unwrap();
            let listing = list(
                &scope,
                ListRequest {
                    repo: repo.clone(),
                    state: "open".into(),
                },
            )
            .unwrap();
            assert_eq!(
                listing["issues_error"],
                "gh request failed; check GitHub CLI access and network connection"
            );
            assert_eq!(listing["prs"][0]["number"], 7);
            assert_eq!(listing["prs"][0]["checks"], "failure");
            assert!(listing["prs"][0].get("url").is_none());
            let details = detail(
                &scope,
                DetailRequest {
                    repo: repo.clone(),
                    kind: "pr".into(),
                    number: 7,
                },
            )
            .unwrap();
            assert_eq!(details["title"], "Example");
            assert!(details["diff_error"].is_string());
            assert!(details["diff"].is_null());
            let outside = list(
                &scope,
                ListRequest {
                    repo: root.join("outside"),
                    state: "open".into(),
                },
            )
            .unwrap_err();
            assert_eq!(outside.code, "outside_workspace");
            let invalid = detail(
                &scope,
                DetailRequest {
                    repo,
                    kind: "https://evil.test".into(),
                    number: 7,
                },
            )
            .unwrap_err();
            assert_eq!(invalid.code, "invalid_request");
            let log = std::fs::read_to_string(root.join("gh.log")).unwrap();
            assert!(log.contains("https://github.com/owner/repo/pull/7"));
            assert!(!log.contains("evil.test"));
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace/repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let repo = git2::Repository::init(&workspace).unwrap();
        repo.remote("origin", "https://github.com/owner/repo.git")
            .unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        git2::Repository::init(outside).unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, r##"#!/bin/sh
printf '%s\n' "$@" >> "$GITPANE_TEST_GITHUB_LOG"
if [ "$1" = issue ] && [ "$2" = list ]; then exit 1; fi
if [ "$1" = pr ] && [ "$2" = list ]; then
  printf '%s' '[{"number":7,"title":"Example","state":"OPEN","statusCheckRollup":[{"state":"FAILURE"}],"url":"https://evil.test/"}]'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  printf '%s' '{"number":7,"title":"Example","body":"text","comments":[]}'
  exit 0
fi
exit 1
"##).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bridge::github::tests::scoped_fake_gh_reports_partial_errors_and_uses_derived_item_url", "--nocapture"])
            .env("PATH", path)
            .env("GITPANE_TEST_GITHUB_ROOT", temp.path())
            .env("GITPANE_TEST_GITHUB_LOG", temp.path().join("gh.log"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
