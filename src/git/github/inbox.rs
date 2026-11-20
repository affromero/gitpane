use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use git2::Repository;
use serde::Deserialize;

use super::{fetch_detail, parse_github_owner_repo, process, safe_repository_pair};
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PullRequest {
    pub number: u64,
    pub title: String,
    pub is_draft: bool,
    repository: RemoteRepository,
    #[serde(skip)]
    pub local_count: usize,
}

impl PullRequest {
    pub(crate) fn repository(&self) -> &str {
        &self.repository.name
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RemoteRepository {
    #[serde(rename = "nameWithOwner")]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Head {
    head_ref_name: String,
    head_repository: Option<HeadRepository>,
    head_repository_owner: Option<Owner>,
}

#[derive(Deserialize)]
struct HeadRepository {
    name: String,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

pub(crate) fn list(paths: &[PathBuf], reviews: bool) -> Result<Vec<PullRequest>, String> {
    let filter = if reviews {
        "--review-requested=@me"
    } else {
        "--author=@me"
    };
    let output = process::run(&[
        "search",
        "prs",
        filter,
        "--state=open",
        "--sort=updated",
        "--limit=50",
        "--json=number,title,isDraft,repository",
    ])?;
    let mut rows: Vec<PullRequest> = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
    if rows.is_empty() {
        return Ok(rows);
    }
    let mut counts = HashMap::new();
    for repo in paths.iter().filter_map(|path| Repository::open(path).ok()) {
        let Ok(remotes) = repo.remotes() else {
            continue;
        };
        let unique: HashSet<_> = remotes
            .iter()
            .filter_map(|name| {
                let remote = repo.find_remote(name.ok().flatten()?).ok()?;
                let (owner, repo) = parse_github_owner_repo(remote.url().ok()?)?;
                Some(format!("{owner}/{repo}").to_ascii_lowercase())
            })
            .collect();
        for slug in unique {
            *counts.entry(slug).or_insert(0) += 1;
        }
    }
    for row in &mut rows {
        row.local_count = counts
            .get(&row.repository().to_ascii_lowercase())
            .copied()
            .unwrap_or(0);
    }
    Ok(rows)
}

fn has_remote(repo: &Repository, target: &str) -> bool {
    repo.remotes().is_ok_and(|names| {
        names
            .iter()
            .filter_map(|name| name.ok().flatten())
            .any(|name| remote_matches(repo, name, target))
    })
}

fn remote_matches(repo: &Repository, name: &str, target: &str) -> bool {
    repo.find_remote(name)
        .ok()
        .and_then(|remote| parse_github_owner_repo(remote.url().ok()?))
        .is_some_and(|(owner, repo)| format!("{owner}/{repo}").eq_ignore_ascii_case(target))
}

#[derive(Clone, Debug)]
pub(crate) struct Preview {
    pub title: String,
    pub body: String,
    pub targets: Vec<(PathBuf, Vec<String>)>,
}

pub(crate) fn inspect(paths: &[PathBuf], base: &str, number: u64) -> Result<Preview> {
    let (owner, repo) = base
        .split_once('/')
        .ok_or_else(|| eyre!("Expected OWNER/REPO"))?;
    if !safe_repository_pair(owner, repo) || number == 0 {
        bail!("Expected a GitHub OWNER/REPO and positive PR number");
    }
    let url = format!("https://github.com/{base}/pull/{number}");
    let detail = fetch_detail(&url, true).map_err(|error| eyre!("{error}"))?;
    let bytes = process::run(&[
        "pr",
        "view",
        &url,
        "--json=headRefName,headRepository,headRepositoryOwner",
    ])
    .map_err(|error| eyre!("{error}"))?;
    let head: Head = serde_json::from_slice(&bytes)?;
    let mut preview = Preview {
        title: detail.title,
        body: detail.body,
        targets: Vec::new(),
    };
    let (Some(repo), Some(owner)) = (head.head_repository, head.head_repository_owner) else {
        return Ok(preview);
    };
    let remote = format!("{}/{}", owner.login, repo.name);
    let matches = matching_worktrees(paths, base, &remote, &head.head_ref_name);
    let panes = crate::session::liveness::tmux_pane_sessions();
    for path in matches {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        let sessions = crate::session::liveness::live_sessions(&canonical, &panes);
        preview.targets.push((path, sessions));
    }
    Ok(preview)
}

fn matching_worktrees(paths: &[PathBuf], base: &str, remote: &str, branch: &str) -> Vec<PathBuf> {
    let expected = format!("refs/heads/{branch}");
    let mut matches = Vec::new();
    for path in paths {
        let Ok(repo) = Repository::open(path) else {
            continue;
        };
        if !has_remote(&repo, base) && !has_remote(&repo, remote) {
            continue;
        }
        let Ok(names) = repo.worktrees() else {
            continue;
        };
        for name in names.iter().filter_map(|name| name.ok().flatten()) {
            let candidate = repo.find_worktree(name).ok().and_then(|worktree| {
                let checkout = Repository::open(worktree.path()).ok()?;
                let head = checkout.head().ok()?;
                let local_ref = head.name().ok()?;
                let upstream = checkout.branch_upstream_remote(local_ref).ok()?;
                let merge = checkout.branch_upstream_merge(local_ref).ok()?;
                if merge.as_str().ok()? != expected
                    || !remote_matches(&checkout, upstream.as_str().ok()?, remote)
                {
                    return None;
                }
                Some(worktree.path().to_path_buf())
            });
            matches.extend(candidate);
        }
    }
    matches.sort();
    matches.dedup();
    matches
}
