use super::*;

use crate::git::github::{self, GhItem};

/// How long a repo's fetched issue/PR data is considered fresh. Selection
/// changes within this window reuse the cache instead of re-spawning `gh`.
const GITHUB_COOLDOWN: Duration = Duration::from_secs(60);

/// Debounce before a settled selection triggers a `gh` fetch, so holding `j`
/// through the repo list doesn't spawn a `gh` process for every transient row.
const GITHUB_SELECT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Cached GitHub state for one repo. Lives on `App` (not `RepoStatus`, which is
/// rebuilt on every 5s poll, nor `RepoEntry`, which is recreated on rescan) so
/// it survives polls and rescans; `RepoId` is path-stable.
#[derive(Default)]
pub(super) struct GithubState {
    /// `origin` parsed to `(owner, repo)`; `None` = not a github.com repo.
    owner_repo: Option<(String, String)>,
    /// A `gh` fetch is currently in flight.
    loading: bool,
    /// Bumped per request so a late result for a superseded request is dropped.
    generation: u64,
    /// When the last fetch attempt completed (drives the freshness cooldown).
    fetched_at: Option<Instant>,
    issues: Vec<GhItem>,
    prs: Vec<GhItem>,
    /// First stderr line of the last failed fetch, if any.
    error: Option<String>,
}

impl App {
    /// The repo whose issues/PRs the panel targets: the parent repo of the
    /// current selection (worktrees and submodules share the parent's origin).
    /// Returns `(id, path, display_name)`.
    fn github_target(&self) -> Option<(RepoId, std::path::PathBuf, String)> {
        if let Some(aw) = &self.active_worktree {
            let idx = self.repo_list.resolve_index(&aw.repo_id)?;
            let entry = &self.repo_list.repos[idx];
            return Some((aw.repo_id.clone(), entry.path.clone(), entry.name.clone()));
        }
        let entry = self.repo_list.selected_repo()?;
        Some((
            RepoId(entry.path.clone()),
            entry.path.clone(),
            entry.name.clone(),
        ))
    }

    /// Open-item count for the selected repo, from the cache. Drives auto-show.
    fn selected_github_open_count(&self) -> usize {
        self.github_target()
            .and_then(|(id, _, _)| self.github_cache.get(&id))
            .map(|s| s.issues.len() + s.prs.len())
            .unwrap_or(0)
    }

    /// Whether the 4th panel should be visible this frame. Off entirely when the
    /// feature is disabled; otherwise a manual `p` override wins, and the default
    /// auto-shows only when the selected repo has open issues or PRs.
    pub(super) fn show_github_panel(&self) -> bool {
        if !self.config.github.enabled {
            return false;
        }
        match self.github_forced {
            Some(forced) => forced,
            None => self.selected_github_open_count() > 0,
        }
    }

    /// Selection changed: reset any manual override and schedule a debounced
    /// fetch for the newly selected repo.
    pub(super) fn github_touch_selection(&mut self) {
        if !self.config.github.enabled {
            return;
        }
        self.github_forced = None;
        self.github_select_gen = self.github_select_gen.wrapping_add(1);
        let generation = self.github_select_gen;
        let tx = self.action_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(GITHUB_SELECT_DEBOUNCE).await;
            let _ = tx.send(Action::GithubSelectionSettled(generation));
        });
    }

    /// Fired after the selection debounce: fetch the current repo and refresh
    /// the view, unless a newer selection has superseded this one.
    pub(super) fn github_selection_settled(&mut self, generation: u64) {
        if generation != self.github_select_gen {
            return;
        }
        self.request_github(false);
        self.refresh_github_panel();
    }

    /// Manual `p` toggle: flip the panel's forced visibility. Forcing it open on
    /// a repo with no cached data kicks off a fetch.
    pub(super) fn toggle_github_panel(&mut self) {
        if !self.config.github.enabled {
            self.error_message = Some((
                "GitHub panel disabled ([github] enabled = false)".to_string(),
                Instant::now(),
            ));
            return;
        }
        let now_visible = self.show_github_panel();
        self.github_forced = Some(!now_visible);
        if self.github_forced == Some(true) {
            self.request_github(true);
            self.refresh_github_panel();
        }
    }

    /// Spawn a `gh` fetch for the selected repo when due. `force` bypasses the
    /// freshness cooldown (used by the manual toggle and refresh). No-op when the
    /// feature is off, `gh` is absent, a fetch is already in flight, the data is
    /// still fresh, or the repo has no github.com origin.
    pub(super) fn request_github(&mut self, force: bool) {
        if !self.config.github.enabled || !github::gh_available() {
            return;
        }
        let Some((id, path, _)) = self.github_target() else {
            return;
        };
        let now = Instant::now();
        let entry = self.github_cache.entry(id.clone()).or_default();
        if entry.loading {
            return;
        }
        if !force
            && let Some(at) = entry.fetched_at
            && now.saturating_duration_since(at) < GITHUB_COOLDOWN
        {
            return;
        }
        if entry.owner_repo.is_none() {
            entry.owner_repo = github::origin_owner_repo(&path);
        }
        let Some((owner, repo)) = entry.owner_repo.clone() else {
            // Not a github.com repo: record the attempt so we don't re-probe the
            // remote on every settle, and never spawn `gh`.
            entry.fetched_at = Some(now);
            return;
        };
        entry.loading = true;
        entry.generation = entry.generation.wrapping_add(1);
        let generation = entry.generation;
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = github::fetch(&owner, &repo, "open");
            let _ = tx.send(Action::GitHubFetched {
                repo_id: id,
                generation,
                result,
            });
        });
    }

    /// Apply a completed fetch to the cache and, if it is for the repo on screen,
    /// refresh the panel view. Stale results (superseded generation) are dropped.
    pub(super) fn github_fetched(
        &mut self,
        repo_id: RepoId,
        generation: u64,
        result: Result<github::GithubData, String>,
    ) {
        if let Some(entry) = self.github_cache.get_mut(&repo_id) {
            if entry.generation != generation {
                return;
            }
            entry.loading = false;
            entry.fetched_at = Some(Instant::now());
            match result {
                Ok(data) => {
                    entry.issues = data.issues;
                    entry.prs = data.prs;
                    entry.error = None;
                }
                Err(e) => entry.error = Some(e),
            }
        }
        if self.github_target().map(|t| t.0) == Some(repo_id) {
            self.refresh_github_panel();
        }
    }

    /// Push the selected repo's cached state into the panel component.
    pub(super) fn refresh_github_panel(&mut self) {
        let Some((id, _, name)) = self.github_target() else {
            return;
        };
        // No `gh` means no fetch will ever populate the cache; say so plainly
        // instead of a "Loading…" that never resolves (only visible if the user
        // force-opens the panel, since auto-show needs a non-zero count).
        if !github::gh_available() {
            self.github_panel
                .set_error(&name, "gh CLI not found on PATH".to_string());
            return;
        }
        match self.github_cache.get(&id) {
            None => self.github_panel.set_loading(&name),
            Some(s) if s.loading && s.fetched_at.is_none() => self.github_panel.set_loading(&name),
            Some(s) if s.owner_repo.is_none() && s.fetched_at.is_some() => {
                self.github_panel.set_not_github(&name)
            }
            Some(s) if s.error.is_some() => self
                .github_panel
                .set_error(&name, s.error.clone().unwrap_or_default()),
            Some(s) => self
                .github_panel
                .set_data(s.issues.clone(), s.prs.clone(), &name),
        }
    }

    /// Drop cached GitHub state for repos that vanished from the list, mirroring
    /// how `DiscoverNewRepos` prunes the other per-repo maps.
    pub(super) fn prune_github_cache(&mut self, removed: &[std::path::PathBuf]) {
        for path in removed {
            self.github_cache.remove(&RepoId(path.clone()));
        }
    }
}
