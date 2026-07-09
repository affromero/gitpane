//! GitHub issue/PR peek via the `gh` CLI.
//!
//! gitpane shells out to `gh` here, mirroring how the rest of the app shells out
//! to `git`. That means `gh`'s own auth, host, and enterprise config are
//! inherited for free: there is no token handling, no HTTP client, and no
//! rate-limit bookkeeping in gitpane. A repo is a candidate only when its
//! `origin` remote points at github.com; everything else returns `None` and the
//! panel stays hidden.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

/// Rolled-up CI/checks result for a pull request, from `gh`'s
/// `statusCheckRollup`. `None` on an item means no checks (every issue, and a
/// PR with an empty rollup).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckState {
    /// At least one check succeeded and none failed or are still running.
    Success,
    /// At least one check failed, errored, timed out, was cancelled, or needs
    /// action. Failure dominates: it wins over any pending or success.
    Failure,
    /// No failure, but at least one check is still queued or in progress.
    Pending,
}

/// One issue or pull request, in the shape the panel renders.
#[derive(Clone, Debug)]
pub(crate) struct GhItem {
    pub number: u64,
    pub title: String,
    /// "OPEN" | "CLOSED" | "MERGED" as reported by `gh`. Used to mark
    /// non-open rows when the closed/all filter is active.
    pub state: String,
    /// Only meaningful for pull requests; always false for issues.
    pub is_draft: bool,
    pub author: String,
    /// ISO-8601 timestamp (`updatedAt`); the panel shows its date part.
    pub updated_at: String,
    /// Web URL, opened in the browser on Enter.
    pub url: String,
    /// Rolled-up CI/checks state for a PR; `None` for issues or an empty rollup.
    pub checks: Option<CheckState>,
}

/// Open issues and PRs for one repo, returned by [`fetch`].
#[derive(Clone, Debug, Default)]
pub(crate) struct GithubData {
    pub issues: Vec<GhItem>,
    pub prs: Vec<GhItem>,
}

/// One comment on an issue or PR, for the detail pane.
#[derive(Clone, Debug)]
pub(crate) struct GhComment {
    pub author: String,
    pub body: String,
    /// ISO-8601 timestamp; the pane shows its date part.
    pub created_at: String,
}

/// The body and comment thread of one issue or PR, fetched on demand.
#[derive(Clone, Debug)]
pub(crate) struct ItemDetail {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub body: String,
    pub comments: Vec<GhComment>,
    /// Unified diff of a PR's changed files (PRs only; `None` for issues or when
    /// the diff can't be fetched).
    pub diff: Option<String>,
}

/// Whether the `gh` executable is available on PATH. Probed once (via
/// `gh --version`) and cached for the process lifetime, mirroring
/// [`crate::git::git_available`]. When absent, gitpane keeps its three panels
/// and never spawns `gh`.
pub(crate) fn gh_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("gh")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Resolve a repo's `origin` remote to `(owner, repo)` when it is a github.com
/// remote, else `None`. Reads the remote URL through git2 (already a dep) and
/// parses the common SSH/HTTPS forms.
pub(crate) fn origin_owner_repo(path: &Path) -> Option<(String, String)> {
    let repo = git2::Repository::open(path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url().ok()?;
    parse_github_owner_repo(url)
}

/// Parse `owner`/`repo` out of a github.com remote URL. Supports:
/// `git@github.com:owner/repo(.git)`, `ssh://git@github.com/owner/repo(.git)`,
/// `https://github.com/owner/repo(.git)`, and the `http`/`git` schemes. Any
/// other host (GitLab, an enterprise GitHub, a bare path) returns `None`.
fn parse_github_owner_repo(url: &str) -> Option<(String, String)> {
    let rest = [
        "git@github.com:",
        "ssh://git@github.com/",
        "https://github.com/",
        "http://github.com/",
        "git://github.com/",
    ]
    .into_iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;

    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    let mut parts = rest.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Fetch open issues and PRs for `owner/repo` in one call each via `gh`.
/// `state` is passed to `--state` ("open", "closed", or "all"). A failure on one
/// list (e.g. issues disabled on the repo) does not sink the other; only when
/// both calls fail is an `Err` returned, carrying the first error.
pub(crate) fn fetch(owner: &str, repo: &str, state: &str) -> Result<GithubData, String> {
    let issues = fetch_list(owner, repo, false, state);
    let prs = fetch_list(owner, repo, true, state);
    match (issues, prs) {
        (Err(e), Err(_)) => Err(e),
        (issues, prs) => Ok(GithubData {
            issues: issues.unwrap_or_default(),
            prs: prs.unwrap_or_default(),
        }),
    }
}

/// One `gh issue list` / `gh pr list` invocation, parsed into [`GhItem`]s.
fn fetch_list(owner: &str, repo: &str, is_pr: bool, state: &str) -> Result<Vec<GhItem>, String> {
    let sub = if is_pr { "pr" } else { "issue" };
    // PRs carry `isDraft`; issues do not, so request the field only for PRs.
    let fields = if is_pr {
        "number,title,state,author,updatedAt,url,isDraft,statusCheckRollup"
    } else {
        "number,title,state,author,updatedAt,url"
    };
    let repo_arg = format!("{owner}/{repo}");
    let output = Command::new("gh")
        .args([
            sub, "list", "-R", &repo_arg, "--state", state, "--limit", "50", "--json", fields,
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "gh is not installed or not on PATH".to_string()
            } else {
                e.to_string()
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("gh failed")
            .trim()
            .to_string();
        return Err(msg);
    }

    let raw: Vec<RawItem> = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    Ok(raw.into_iter().map(RawItem::into_item).collect())
}

/// Fetch the body and comment thread of one issue/PR by its web URL. `is_pr`
/// picks the `gh issue view` vs `gh pr view` subcommand; both accept the URL.
pub(crate) fn fetch_detail(url: &str, is_pr: bool) -> Result<ItemDetail, String> {
    let sub = if is_pr { "pr" } else { "issue" };
    let output = Command::new("gh")
        .args([
            sub,
            "view",
            url,
            "--json",
            "number,title,author,body,comments",
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "gh is not installed or not on PATH".to_string()
            } else {
                e.to_string()
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("gh failed")
            .trim()
            .to_string();
        return Err(msg);
    }

    let raw: RawDetail = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let mut detail = raw.into_detail();
    if is_pr {
        detail.diff = pr_diff(url);
    }
    Ok(detail)
}

/// Best-effort unified diff for a PR (`gh pr diff <url>`). Returns `None` on any
/// failure — the diff is a bonus shown under the body and comments.
fn pr_diff(url: &str) -> Option<String> {
    let output = Command::new("gh").args(["pr", "diff", url]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[derive(Deserialize)]
struct RawAuthor {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawItem {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    status_check_rollup: Vec<RawCheck>,
}

/// One entry of a PR's `statusCheckRollup`. `gh` mixes two shapes: a modern
/// `CheckRun` (a `status` that is `COMPLETED` etc., plus a `conclusion` once
/// complete) and a legacy `StatusContext` (a single `state`). We read whichever
/// fields are present; missing ones stay `None`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheck {
    /// CheckRun lifecycle: QUEUED | IN_PROGRESS | COMPLETED | …
    #[serde(default)]
    status: Option<String>,
    /// CheckRun result once COMPLETED: SUCCESS | FAILURE | NEUTRAL | …
    #[serde(default)]
    conclusion: Option<String>,
    /// StatusContext result: SUCCESS | PENDING | FAILURE | ERROR | EXPECTED.
    #[serde(default)]
    state: Option<String>,
}

/// Collapse a PR's check entries to a single [`CheckState`], mirroring GitHub's
/// own rollup precedence: any failure ⇒ Failure, else any still-running ⇒
/// Pending, else any success ⇒ Success, else `None` (empty, or only
/// neutral/skipped checks).
fn rollup_state(checks: &[RawCheck]) -> Option<CheckState> {
    let mut any_pending = false;
    let mut any_success = false;
    for c in checks {
        if let Some(state) = c.state.as_deref() {
            // Legacy StatusContext.
            match state.to_ascii_uppercase().as_str() {
                "FAILURE" | "ERROR" => return Some(CheckState::Failure),
                "PENDING" | "EXPECTED" => any_pending = true,
                "SUCCESS" => any_success = true,
                _ => {}
            }
        } else {
            // Modern CheckRun: not COMPLETED means it is still running.
            let completed = c
                .status
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("COMPLETED"));
            if !completed {
                any_pending = true;
                continue;
            }
            match c
                .conclusion
                .as_deref()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str()
            {
                "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
                | "CANCELLED" => return Some(CheckState::Failure),
                "SUCCESS" => any_success = true,
                // NEUTRAL / SKIPPED / STALE / unknown: not blocking, not counted.
                _ => {}
            }
        }
    }
    if any_pending {
        Some(CheckState::Pending)
    } else if any_success {
        Some(CheckState::Success)
    } else {
        None
    }
}

impl RawItem {
    fn into_item(self) -> GhItem {
        GhItem {
            number: self.number,
            title: self.title,
            state: self.state,
            is_draft: self.is_draft,
            author: self.author.map(|a| a.login).unwrap_or_default(),
            updated_at: self.updated_at,
            url: self.url,
            checks: rollup_state(&self.status_check_rollup),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    comments: Vec<RawComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
}

impl RawDetail {
    fn into_detail(self) -> ItemDetail {
        ItemDetail {
            number: self.number,
            title: self.title,
            author: self.author.map(|a| a.login).unwrap_or_default(),
            body: self.body,
            diff: None,
            comments: self
                .comments
                .into_iter()
                .map(|c| GhComment {
                    author: c.author.map(|a| a.login).unwrap_or_default(),
                    body: c.body,
                    created_at: c.created_at,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_scp_form() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:affromero/gitpane.git"),
            Some(("affromero".into(), "gitpane".into()))
        );
    }

    #[test]
    fn parses_ssh_url_form() {
        assert_eq!(
            parse_github_owner_repo("ssh://git@github.com/affromero/gitpane.git"),
            Some(("affromero".into(), "gitpane".into()))
        );
    }

    #[test]
    fn parses_https_with_and_without_git_suffix() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/affromero/gitpane.git"),
            Some(("affromero".into(), "gitpane".into()))
        );
        assert_eq!(
            parse_github_owner_repo("https://github.com/affromero/gitpane"),
            Some(("affromero".into(), "gitpane".into()))
        );
    }

    #[test]
    fn rejects_non_github_hosts() {
        assert_eq!(
            parse_github_owner_repo("git@gitlab.com:owner/repo.git"),
            None
        );
        assert_eq!(
            parse_github_owner_repo("https://bitbucket.org/owner/repo"),
            None
        );
        assert_eq!(
            parse_github_owner_repo("https://github.enterprise.io/owner/repo"),
            None
        );
    }

    #[test]
    fn rejects_incomplete_paths() {
        assert_eq!(parse_github_owner_repo("https://github.com/owner"), None);
        assert_eq!(parse_github_owner_repo("git@github.com:"), None);
        assert_eq!(
            parse_github_owner_repo("https://github.com/owner/repo/extra"),
            None
        );
    }

    #[test]
    fn deserializes_gh_json_shape() {
        let json = r#"[
            {"number":7,"title":"Bug","state":"OPEN","author":{"login":"alice"},"updatedAt":"2026-01-02T03:04:05Z","url":"https://github.com/o/r/issues/7"},
            {"number":8,"title":"Feature","state":"OPEN","author":null,"updatedAt":"2026-01-03T00:00:00Z","url":"https://github.com/o/r/pull/8","isDraft":true}
        ]"#;
        let raw: Vec<RawItem> = serde_json::from_str(json).unwrap();
        let items: Vec<GhItem> = raw.into_iter().map(RawItem::into_item).collect();
        assert_eq!(items[0].number, 7);
        assert_eq!(items[0].author, "alice");
        assert!(!items[0].is_draft);
        assert_eq!(items[1].author, ""); // null author → empty
        assert!(items[1].is_draft);
        // No statusCheckRollup field present ⇒ no checks.
        assert_eq!(items[0].checks, None);
        assert_eq!(items[1].checks, None);
    }

    fn rollup(json: &str) -> Option<CheckState> {
        let checks: Vec<RawCheck> = serde_json::from_str(json).unwrap();
        rollup_state(&checks)
    }

    #[test]
    fn rollup_empty_is_none() {
        assert_eq!(rollup("[]"), None);
    }

    #[test]
    fn rollup_all_success_is_success() {
        let json = r#"[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"StatusContext","state":"SUCCESS"}
        ]"#;
        assert_eq!(rollup(json), Some(CheckState::Success));
    }

    #[test]
    fn rollup_any_failure_dominates_success_and_pending() {
        // One failing check outranks a passing one and an in-progress one.
        let json = r#"[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":null},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}
        ]"#;
        assert_eq!(rollup(json), Some(CheckState::Failure));
    }

    #[test]
    fn rollup_running_check_is_pending_when_nothing_failed() {
        let json = r#"[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"CheckRun","status":"QUEUED","conclusion":null}
        ]"#;
        assert_eq!(rollup(json), Some(CheckState::Pending));
    }

    #[test]
    fn rollup_legacy_status_context_error_is_failure() {
        assert_eq!(
            rollup(r#"[{"__typename":"StatusContext","state":"ERROR"}]"#),
            Some(CheckState::Failure)
        );
        assert_eq!(
            rollup(r#"[{"__typename":"StatusContext","state":"PENDING"}]"#),
            Some(CheckState::Pending)
        );
    }

    #[test]
    fn rollup_only_neutral_or_skipped_is_none() {
        let json = r#"[
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"NEUTRAL"},
            {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}
        ]"#;
        assert_eq!(rollup(json), None);
    }

    #[test]
    fn deserializes_detail_shape() {
        let json = r#"{
            "number": 12,
            "title": "A bug",
            "author": {"login": "alice"},
            "body": "It broke.",
            "comments": [
                {"author": {"login": "bob"}, "body": "Confirmed", "createdAt": "2026-02-01T00:00:00Z"},
                {"author": null, "body": "ghost", "createdAt": "2026-02-02T00:00:00Z"}
            ]
        }"#;
        let raw: RawDetail = serde_json::from_str(json).unwrap();
        let detail = raw.into_detail();
        assert_eq!(detail.number, 12);
        assert_eq!(detail.author, "alice");
        assert_eq!(detail.body, "It broke.");
        assert_eq!(detail.comments.len(), 2);
        assert_eq!(detail.comments[0].author, "bob");
        assert_eq!(detail.comments[1].author, ""); // null author → empty
    }
}
