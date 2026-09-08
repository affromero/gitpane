use super::*;

use crate::git::github;
use std::sync::atomic::AtomicU64;

/// How long a repo's fetched issue/PR data is considered fresh. Selection
/// changes within this window reuse the cache instead of re-spawning `gh`.
const GITHUB_COOLDOWN: Duration = Duration::from_secs(60);

/// Debounce before a settled selection triggers a `gh` fetch, so holding `j`
/// through the repo list doesn't spawn a `gh` process for every transient row.
const GITHUB_SELECT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Which issues/PRs the panel lists. Cycled with `c`; reset to `Open` on nav.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(super) enum GithubStateFilter {
    #[default]
    Open,
    All,
    Closed,
}

impl GithubStateFilter {
    fn next(self) -> Self {
        match self {
            Self::Open => Self::All,
            Self::All => Self::Closed,
            Self::Closed => Self::Open,
        }
    }

    /// The `--state` argument passed to `gh`.
    fn gh_state(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::All => "all",
            Self::Closed => "closed",
        }
    }

    /// Short label shown in the panel title.
    fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::All => "all",
            Self::Closed => "closed",
        }
    }
}

/// Cached GitHub state for one repo. Lives on `App` (not `RepoStatus`, which is
/// rebuilt on every 5s poll, nor `RepoEntry`, which is recreated on rescan) so
/// it survives polls and rescans; `RepoId` is path-stable.
#[derive(Default)]
pub(super) struct GithubState {
    /// `origin` parsed to `(owner, repo)`; `None` = not a github.com repo.
    owner_repo: Option<(String, String)>,
    remote_checked_at: Option<Instant>,
    snapshots: HashMap<GithubStateFilter, GithubSnapshot>,
}

#[derive(Default)]
struct GithubSnapshot {
    /// The request ID also identifies its filter through this snapshot.
    loading: Option<u64>,
    /// When the last fetch attempt completed (drives the freshness cooldown).
    fetched_at: Option<Instant>,
    data: Option<github::GithubData>,
    /// First stderr line of the last failed fetch, if any.
    error: Option<String>,
}

impl GithubState {
    /// Reserve one request per filter. IDs remain unique after a repository is
    /// removed and re-added, so a response from its former cache cannot land.
    fn begin_request(
        &mut self,
        filter: GithubStateFilter,
        force: bool,
        now: Instant,
    ) -> Option<u64> {
        static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
        let snapshot = self.snapshots.entry(filter).or_default();
        if snapshot.loading.is_some()
            || (!force
                && snapshot
                    .fetched_at
                    .is_some_and(|at| now.saturating_duration_since(at) < GITHUB_COOLDOWN))
        {
            return None;
        }
        let generation = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        snapshot.loading = Some(generation);
        Some(generation)
    }

    fn complete_request(&mut self, generation: u64, result: Result<github::GithubData, String>) {
        let Some(snapshot) = self
            .snapshots
            .values_mut()
            .find(|s| s.loading == Some(generation))
        else {
            return;
        };
        snapshot.loading = None;
        snapshot.fetched_at = Some(Instant::now());
        match result {
            Ok(data) => {
                snapshot.data = Some(data);
                snapshot.error = None;
            }
            Err(error) => snapshot.error = Some(error),
        }
    }

    fn open_count(&self) -> usize {
        self.snapshots
            .get(&GithubStateFilter::Open)
            .and_then(|snapshot| snapshot.data.as_ref())
            .map(|data| data.issues.len() + data.prs.len())
            .unwrap_or(0)
    }
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
    /// Uses the cached open count (not the displayed rows) so viewing closed/all
    /// items never changes whether the panel auto-shows.
    fn selected_github_open_count(&self) -> usize {
        self.github_target()
            .and_then(|(id, _, _)| self.github_cache.get(&id))
            .map(GithubState::open_count)
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
        self.github_state_filter = GithubStateFilter::Open;
        self.retarget_github_panel();
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
        let filter = self.github_state_filter;
        let now = Instant::now();
        let entry = self.github_cache.entry(id.clone()).or_default();
        if entry.owner_repo.is_none() {
            if !force
                && entry
                    .remote_checked_at
                    .is_some_and(|at| now.saturating_duration_since(at) < GITHUB_COOLDOWN)
            {
                return;
            }
            entry.owner_repo = github::github_owner_repo(&path);
            entry.remote_checked_at = Some(now);
        }
        let Some((owner, repo)) = entry.owner_repo.clone() else {
            // Not a github.com repo: record the attempt so we don't re-probe the
            // remote on every settle, and never spawn `gh`.
            return;
        };
        let Some(generation) = entry.begin_request(filter, force, now) else {
            return;
        };
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = github::fetch(&owner, &repo, filter.gh_state());
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
            entry.complete_request(generation, result);
        }
        if self.github_target().map(|t| t.0) == Some(repo_id) {
            self.refresh_github_panel();
        }
    }

    /// Push the selected repo's cached state into the panel component.
    pub(super) fn refresh_github_panel(&mut self) {
        self.retarget_github_panel();
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
        let label = self.github_state_filter.label();
        match self.github_cache.get(&id) {
            None => self.github_panel.set_loading(&name),
            Some(s) if s.owner_repo.is_none() && s.remote_checked_at.is_some() => {
                self.github_panel.set_not_github(&name)
            }
            Some(s) => match s.snapshots.get(&self.github_state_filter) {
                Some(snapshot) if snapshot.error.is_some() => self
                    .github_panel
                    .set_error(&name, snapshot.error.clone().unwrap_or_default()),
                Some(GithubSnapshot {
                    data: Some(data), ..
                }) => {
                    self.github_panel
                        .set_data(data.issues.clone(), data.prs.clone(), &name, label)
                }
                _ => self.github_panel.set_loading(&name),
            },
        }
    }

    /// Clear old rows and pending detail immediately when the target changes,
    /// including two distinct repositories with the same display name.
    fn retarget_github_panel(&mut self) {
        let target = self.github_target();
        self.github_panel.set_target(
            target.as_ref().map(|(id, _, _)| id.clone()),
            target
                .as_ref()
                .map(|(_, _, name)| name.as_str())
                .unwrap_or(""),
            self.github_state_filter.label(),
        );
    }

    /// Cycle the panel's state filter (open → all → closed) and refetch. Sent by
    /// `c` in the panel. The auto-show count is unaffected (see `github_fetched`).
    pub(super) fn cycle_github_state_filter(&mut self) {
        if !self.config.github.enabled {
            return;
        }
        self.github_state_filter = self.github_state_filter.next();
        self.request_github(true);
        self.refresh_github_panel();
    }

    /// Drop cached GitHub state for repos that vanished from the list, mirroring
    /// how `DiscoverNewRepos` prunes the other per-repo maps.
    pub(super) fn prune_github_cache(&mut self, removed: &[std::path::PathBuf]) {
        for path in removed {
            self.github_cache.remove(&RepoId(path.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(title: &str, state: &str) -> github::GithubData {
        github::GithubData {
            issues: vec![github::GhItem {
                number: 1,
                title: title.into(),
                state: state.into(),
                is_draft: false,
                author: "alice".into(),
                updated_at: String::new(),
                url: "https://github.com/owner/repo/issues/1".into(),
                checks: None,
            }],
            prs: vec![],
        }
    }

    #[test]
    fn closed_response_does_not_become_open_data_after_navigation() {
        let mut app = App::new(Config {
            root_dirs: vec![],
            ..Config::default()
        });
        let id = RepoId("/repo".into());
        let mut cache = GithubState::default();
        let request = cache
            .begin_request(GithubStateFilter::Closed, false, Instant::now())
            .unwrap();
        app.github_cache.insert(id.clone(), cache);
        app.github_state_filter = GithubStateFilter::Open;
        app.github_fetched(id.clone(), request, Ok(data("Closed issue", "CLOSED")));
        let cache = &app.github_cache[&id];
        assert_eq!(
            cache.open_count(),
            0,
            "closed response changed the open count"
        );
        assert_eq!(
            cache.snapshots[&GithubStateFilter::Closed]
                .data
                .as_ref()
                .unwrap()
                .issues[0]
                .title,
            "Closed issue"
        );
    }

    #[test]
    fn filter_changes_load_independently_and_keep_out_of_order_responses_separate() {
        let mut cache = GithubState::default();
        let open = cache
            .begin_request(GithubStateFilter::Open, false, Instant::now())
            .unwrap();
        let closed = cache
            .begin_request(GithubStateFilter::Closed, false, Instant::now())
            .unwrap();
        cache.complete_request(closed, Ok(data("Closed issue", "CLOSED")));
        assert_eq!(cache.open_count(), 0);
        cache.complete_request(open, Ok(data("Open issue", "OPEN")));
        assert_eq!(cache.open_count(), 1);
        for (filter, title) in [
            (GithubStateFilter::Open, "Open issue"),
            (GithubStateFilter::Closed, "Closed issue"),
        ] {
            assert_eq!(
                cache.snapshots[&filter].data.as_ref().unwrap().issues[0].title,
                title
            );
        }
    }

    #[test]
    fn fresh_closed_results_do_not_suppress_loading_open_results() {
        let mut cache = GithubState::default();
        let request = cache
            .begin_request(GithubStateFilter::Closed, false, Instant::now())
            .unwrap();
        cache.complete_request(request, Ok(data("Closed issue", "CLOSED")));
        let open = cache
            .begin_request(GithubStateFilter::Open, false, Instant::now())
            .unwrap();
        cache.complete_request(open, Ok(data("Open issue", "OPEN")));
        assert_eq!(cache.open_count(), 1);
        assert!(
            cache
                .begin_request(GithubStateFilter::Open, false, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn readded_repository_rejects_response_from_its_previous_cache() {
        let mut previous = GithubState::default();
        let old_request = previous
            .begin_request(GithubStateFilter::Open, false, Instant::now())
            .unwrap();
        let mut current = GithubState::default();
        let request = current
            .begin_request(GithubStateFilter::Open, false, Instant::now())
            .unwrap();
        current.complete_request(old_request, Ok(data("Old result", "OPEN")));
        assert_eq!(current.open_count(), 0);
        current.complete_request(request, Ok(data("Current result", "OPEN")));
        assert_eq!(
            current.snapshots[&GithubStateFilter::Open]
                .data
                .as_ref()
                .unwrap()
                .issues[0]
                .title,
            "Current result"
        );
    }
}
