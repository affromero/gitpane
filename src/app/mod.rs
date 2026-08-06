use color_eyre::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::action::Action;
use crate::components::Component;
use crate::components::confirm_dialog::ConfirmDialog;
use crate::components::context_menu::ContextMenu;
use crate::components::file_list::FileList;
use crate::components::git_graph::GitGraph;
use crate::components::github_panel::GithubPanel;
use crate::components::graph_menu::context_menu::GraphContextMenu;
use crate::components::graph_menu::filter_picker::GraphFilterPicker;
use crate::components::path_input::PathInput;
use crate::components::picker::Picker;
use crate::components::repo_list::RepoEntry;
use crate::components::repo_list::RepoList;
use crate::components::status_bar::StatusBar;
use crate::components::theme_picker::ThemePicker;
use crate::config::BranchFilter;
use crate::config::Config;
use crate::config::UpdatePosition;
use crate::event::Event;
use crate::git::graph::GraphOptions;
use crate::git::scanner;
use crate::git::status::RepoStatus;
use crate::repo_id::RepoId;
use crate::theme::{Theme, discover_all_theme_names, load_theme};
use crate::tui::Tui;
use crate::watcher::RepoWatcher;

mod actions;
mod actions_extra;
mod github;
mod input;
mod launch;
mod render;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusPanel {
    Repos,
    Changes,
    Graph,
    GitHub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortOrder {
    Alphabetical,
    ReverseAlphabetical,
    DirtyFirst,
}

impl SortOrder {
    fn next(self) -> Self {
        match self {
            Self::Alphabetical => Self::ReverseAlphabetical,
            Self::ReverseAlphabetical => Self::DirtyFirst,
            Self::DirtyFirst => Self::Alphabetical,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "A-Z",
            Self::ReverseAlphabetical => "Z-A",
            Self::DirtyFirst => "Dirty",
        }
    }
}

/// RAII guard that sends `StatusQueryDone` if the spawned task exits
/// without sending a completion message (e.g., on panic). The guard's
/// `Drop` uses `UnboundedSender::send` which is non-blocking, so it
/// is safe to call from a synchronous `Drop`.
struct StatusGuard {
    id: RepoId,
    tx: UnboundedSender<Action>,
    completed: bool,
}

impl StatusGuard {
    fn new(id: RepoId, tx: UnboundedSender<Action>) -> Self {
        Self {
            id,
            tx,
            completed: false,
        }
    }

    /// Mark the guard as completed so `Drop` won't send cleanup.
    /// Consumes self to prevent accidental reuse.
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for StatusGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.tx.send(Action::StatusQueryDone(self.id.clone()));
        }
    }
}

/// RAII guard for git operations (push/pull/submodule) that set `git_op = true`.
/// If the spawned task panics without sending `GitOpComplete` or `RefreshRepo`,
/// the guard sends `RefreshRepo` to trigger a status query that clears `git_op`.
struct GitOpGuard {
    id: RepoId,
    tx: UnboundedSender<Action>,
    completed: bool,
}

impl GitOpGuard {
    fn new(id: RepoId, tx: UnboundedSender<Action>) -> Self {
        Self {
            id,
            tx,
            completed: false,
        }
    }

    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for GitOpGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.tx.send(Action::RefreshRepo(self.id.clone()));
        }
    }
}

pub(crate) struct App {
    config: Config,
    should_quit: bool,
    repo_list: RepoList,
    file_list: FileList,
    git_graph: GitGraph,
    graph_context_menu: GraphContextMenu,
    graph_filter_picker: GraphFilterPicker,
    github_panel: GithubPanel,
    confirm_dialog: ConfirmDialog,
    context_menu: ContextMenu,
    path_input: PathInput,
    status_bar: StatusBar,
    theme_picker: ThemePicker,
    picker: Picker,
    /// What the picker is currently choosing for, parked until it resolves.
    pending_pick: Option<PendingPick>,
    focus: FocusPanel,
    sort_order: SortOrder,
    action_tx: UnboundedSender<Action>,
    action_rx: UnboundedReceiver<Action>,
    repo_area: Rect,
    changes_area: Rect,
    graph_area: Rect,
    /// Area of the optional 4th (GitHub) panel; `Rect::default()` when hidden.
    github_area: Rect,
    /// Cached issue/PR data per repo (see [`github::GithubState`]).
    github_cache: HashMap<RepoId, github::GithubState>,
    /// Manual override for the GitHub panel: `None` auto (show when the repo has
    /// open items), `Some(true)`/`Some(false)` force open/closed. Reset on nav.
    github_forced: Option<bool>,
    /// Whether the GitHub panel was drawn last frame; gates focus cycling and
    /// mouse focus so they never land on a hidden panel.
    github_visible: bool,
    /// Monotonic selection counter used to debounce GitHub fetches.
    github_select_gen: u64,
    /// Which issues/PRs the panel lists (open/all/closed); reset on nav.
    github_state_filter: github::GithubStateFilter,
    error_message: Option<(String, Instant)>,
    success_message: Option<(String, Instant)>,
    /// Long-lived system clipboard. Kept alive so the platform backend's
    /// selection-serving thread survives past a single copy. Creating and
    /// dropping a `Clipboard` per copy kills that thread immediately, which on
    /// Wayland (X11 fallback) or X11 without a clipboard manager leaves the
    /// clipboard empty right after "Copied to clipboard". Lazily created on
    /// first use; reset on error so a stale connection is retried fresh.
    clipboard: Option<arboard::Clipboard>,
    /// Which border is being dragged: 0 = repos|changes, 1 = changes|graph,
    /// 2 = graph|github (only when the 4th panel is shown).
    dragging_border: Option<u8>,
    /// Fraction of the layout axis for each border (0.0..1.0). Index 0 is the
    /// repos/changes split, 1 the changes/graph split, and 2 the graph/github
    /// split (live only when the GitHub panel is shown). Applies to width in
    /// horizontal mode, height in vertical mode.
    border_frac: [f64; 3],
    /// True when the layout is horizontal (side-by-side panels)
    horizontal_layout: bool,
    /// Newer version available (set by background update check)
    update_version: Option<String>,
    /// Where to render the update notification
    update_position: UpdatePosition,
    /// Show the keybindings help overlay
    show_help: bool,
    /// Limits concurrent poll/refresh tasks to avoid CPU spikes
    poll_semaphore: Arc<tokio::sync::Semaphore>,
    /// Repos with an in-flight status query (prevents duplicate spawns)
    pending_status: HashSet<RepoId>,
    /// Repos that changed while a status query was in-flight (re-queued on completion)
    dirty_repos: HashSet<RepoId>,
    /// Last time a status query finished per repo.
    last_refresh: HashMap<RepoId, Instant>,
    /// Repos with a trailing watcher refresh already scheduled.
    refresh_scheduled: HashSet<RepoId>,
    /// When a worktree row is selected, stores context for diff/status routing
    /// and live-polling the worktree's changes.
    active_worktree: Option<ActiveWorktree>,
    /// True while a tmux liveness probe is running, so polls don't pile up
    /// blocking tasks (and results stay in order) if tmux stalls.
    liveness_probe_in_flight: bool,
    theme: Arc<crate::theme::Theme>,
    /// Filesystem watcher owning the notify debouncer. Held so its `Drop`
    /// (which stops the underlying watches) only fires when we deliberately
    /// replace it via `rebuild_watcher`. Shared so the watcher can be built on
    /// a blocking thread and dropped into this slot once ready.
    watcher: Arc<Mutex<Option<RepoWatcher>>>,
    /// Clone of `Tui::event_tx` so `rebuild_watcher` can run outside `run()`.
    tui_event_tx: Option<UnboundedSender<Event>>,
    /// Wall-clock of the last `DiscoverNewRepos` dispatch driven by a
    /// `ReposRootChanged` event. Drives the leading-edge cooldown that
    /// coalesces FS-event storms (e.g., the file deluge during `git clone`).
    last_discovery: Option<Instant>,
    /// True between a cooldown-suppressed `ReposRootChanged` and the
    /// trailing-edge `DiscoverNewRepos` that fires once cooldown expires.
    /// Prevents spawning a second deferred fire while one is already pending.
    discovery_pending: bool,
}

#[derive(Clone)]
struct ActiveWorktree {
    path: std::path::PathBuf,
    repo_id: RepoId,
    display_name: String,
}

/// A launch (`open`/`review`) parked while the placement picker is open, so it
/// can be resumed with the chosen placement.
struct PendingLaunch {
    dir: std::path::PathBuf,
    command: Option<String>,
    base: Option<String>,
    label: &'static str,
}

/// What the shared [`Picker`] is choosing for, so `PickerChose` can route the
/// selected value.
enum PendingPick {
    /// `placement = "ask"`: the value is the chosen tmux placement; resume this launch.
    Launch(PendingLaunch),
    /// "Attach session": the value is the chosen tmux session name.
    GotoSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshDecision {
    Now,
    Later(Duration),
}

fn refresh_decision(last: Option<Instant>, now: Instant, cooldown: Duration) -> RefreshDecision {
    if let Some(last) = last {
        let elapsed = now.saturating_duration_since(last);
        if elapsed < cooldown {
            return RefreshDecision::Later(cooldown - elapsed);
        }
    }

    RefreshDecision::Now
}

fn graph_status_changed(
    previous: Option<&RepoStatus>,
    next: &RepoStatus,
    filter: BranchFilter,
) -> bool {
    let Some(previous) = previous else {
        return true;
    };

    // Reload when a ref the graph actually renders moves. Scope the comparison
    // to the active filter so, e.g., a fetch that advances a remote-tracking ref
    // does not reload a local-only graph. `None` draws only commits reachable
    // from HEAD (covered by `head_oid`), so no ref bucket applies.
    let prev_refs = &previous.refs;
    let next_refs = &next.refs;
    let refs_changed = match filter {
        BranchFilter::All => prev_refs != next_refs,
        BranchFilter::Local => {
            prev_refs.local != next_refs.local || prev_refs.tags != next_refs.tags
        }
        BranchFilter::Remote => {
            prev_refs.remote != next_refs.remote || prev_refs.tags != next_refs.tags
        }
        BranchFilter::None => false,
    };

    refs_changed
        || previous.branch != next.branch
        || previous.head_oid != next.head_oid
        || previous.ahead != next.ahead
        || previous.behind != next.behind
        || previous.stashes.len() != next.stashes.len()
        || previous
            .stashes
            .iter()
            .zip(next.stashes.iter())
            .any(|(previous, next)| previous.oid != next.oid)
}

impl App {
    pub fn new(config: Config) -> Self {
        let repo_paths = scanner::discover_repos(&config);
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let theme = Arc::new(config.theme.clone());

        let mut git_graph = GitGraph::new(theme.clone());
        git_graph.graph_options = GraphOptions {
            branch_filter: config.graph.branches,
            label_max_len: config.graph.label_max_len,
            first_parent: false,
            show_stats: config.graph.show_stats,
            filters: crate::git::graph::GraphFilters::default(),
        };

        let update_position = config.ui.update_position;
        let poll_semaphore = Arc::new(tokio::sync::Semaphore::new(
            config.watch.max_concurrent_polls,
        ));
        // Roots are cloned before `config` moves into `Self` below; the repo
        // list needs them to render each repo's relative display path.
        let roots = config.root_dirs.clone();

        let mut app = Self {
            config,
            should_quit: false,
            repo_list: RepoList::new(repo_paths, roots, theme.clone()),
            file_list: FileList::new(theme.clone()),
            git_graph,
            graph_context_menu: GraphContextMenu::new(theme.clone()),
            graph_filter_picker: GraphFilterPicker::new(theme.clone()),
            github_panel: GithubPanel::new(theme.clone()),
            confirm_dialog: ConfirmDialog::new(theme.clone()),
            context_menu: ContextMenu::new(theme.clone()),
            path_input: PathInput::new(theme.clone()),
            status_bar: StatusBar::new(theme.clone()),
            theme_picker: ThemePicker::new(theme.clone()),
            picker: Picker::new(theme.clone()),
            pending_pick: None,
            focus: FocusPanel::Repos,
            sort_order: SortOrder::Alphabetical,
            action_tx,
            action_rx,
            repo_area: Rect::default(),
            changes_area: Rect::default(),
            graph_area: Rect::default(),
            github_area: Rect::default(),
            github_cache: HashMap::new(),
            github_forced: None,
            github_visible: false,
            github_select_gen: 0,
            github_state_filter: github::GithubStateFilter::default(),
            error_message: None,
            success_message: None,
            clipboard: None,
            dragging_border: None,
            border_frac: [0.25, 0.50, 0.78],
            horizontal_layout: false,
            update_version: None,
            update_position,
            show_help: false,
            poll_semaphore,
            pending_status: HashSet::new(),
            dirty_repos: HashSet::new(),
            last_refresh: HashMap::new(),
            refresh_scheduled: HashSet::new(),
            active_worktree: None,
            liveness_probe_in_flight: false,
            theme,
            watcher: Arc::new(Mutex::new(None)),
            tui_event_tx: None,
            last_discovery: None,
            discovery_pending: false,
        };
        // `discover_repos` orders by basename, but rows are labelled and
        // re-sorted by breadcrumb path. Sorting here means the list the user
        // first sees already matches what the next rescan will produce.
        // `sort_repos` preserves the selection `RepoList::new` put on the
        // pre-sort first repo; at startup the user has selected nothing yet,
        // so land on the first *sorted* row instead.
        app.sort_repos();
        app.repo_list.select_repo_row(0);
        app
    }

    /// Order the repo list by the current `sort_order`. The sort is flat and
    /// case-insensitive: pinned repos take their alphabetical (or dirty)
    /// place like every other row — pinning controls persistence, not
    /// position. `App::new` calls this too, so the order at startup is the
    /// order every later rescan reproduces. The selected row is captured
    /// before the reorder and restored after — a sort changes where rows
    /// live, never which row the user is on.
    fn sort_repos(&mut self) {
        let keep = self.repo_list.selected_row_id();
        match self.sort_order {
            SortOrder::Alphabetical => {
                self.repo_list
                    .repos
                    .sort_by_cached_key(|r| r.display.to_lowercase());
            }
            SortOrder::ReverseAlphabetical => {
                self.repo_list
                    .repos
                    .sort_by_cached_key(|r| std::cmp::Reverse(r.display.to_lowercase()));
            }
            SortOrder::DirtyFirst => {
                self.repo_list.repos.sort_by_cached_key(|r| {
                    let dirty = r.status.as_ref().is_some_and(|s| s.is_dirty);
                    (!dirty, r.display.to_lowercase())
                });
            }
        }
        self.repo_list.resync_rows(keep);
    }
    /// Rebuild the filesystem watcher to match the current `repo_list.repos`
    /// and `config.root_dirs`. The new watcher is constructed before the old
    /// one is dropped, so a transient construction failure leaves the
    /// previous watches intact rather than silently going unwatched.
    ///
    /// On success, the old watcher is dropped by the `Some(w)` assignment.
    /// Events already buffered in the old watcher's routing channel may
    /// still surface briefly after the swap, routed against the stale
    /// repo set. Those late events are absorbed downstream:
    /// `Action::RefreshRepo` no-ops when `resolve_index` misses, and
    /// `Action::DiscoverNewRepos` is idempotent. We rely on those
    /// invariants rather than trying to drain the old channel here.
    /// Rebuild the filesystem watcher off the main loop. Walking each repo to
    /// install per-directory watches can take noticeable wall-clock time on
    /// large checkouts, so we build on a blocking thread and drop the finished
    /// watcher into the shared slot. The UI stays responsive meanwhile, and the
    /// periodic `PollLocal` timer covers the brief window before watches come
    /// online. Storing the new watcher drops the previous one, tearing down its
    /// watches; on error the previous watcher is left in place.
    fn rebuild_watcher(&mut self) {
        let Some(tx) = self.tui_event_tx.clone() else {
            return;
        };
        let repo_paths: Vec<_> = self
            .repo_list
            .repos
            .iter()
            .map(|r| r.path.clone())
            .collect();
        let root_dirs = self.config.root_dirs.clone();
        let debounce_ms = self.config.watch.debounce_ms;
        let exclude_dirs = self.config.watch.watch_exclude_dirs.clone();
        let watch_worktree_dirs = self.config.watch.watch_worktree_dirs;
        let slot = Arc::clone(&self.watcher);
        let repo_count = repo_paths.len();
        tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            match RepoWatcher::new(
                &repo_paths,
                &root_dirs,
                debounce_ms,
                tx,
                &exclude_dirs,
                watch_worktree_dirs,
            ) {
                Ok(w) => {
                    *slot.lock().unwrap() = Some(w);
                    tracing::info!(
                        "filesystem watcher ready: {} repos in {:?}",
                        repo_count,
                        started.elapsed()
                    );
                }
                Err(e) => tracing::warn!(
                    "Failed to rebuild filesystem watcher; keeping previous watches: {}",
                    e
                ),
            }
        });
    }
    /// Auto-load graph + file list for the selected repo.
    fn sync_selection(&mut self) {
        // Retarget the GitHub panel onto the (new) selection, debounced.
        self.github_touch_selection();
        // An active worktree/submodule context owns the detail panels. A sort,
        // rescan, or discovery must refresh *that* path, not reload the selected
        // parent row over it (a submodule is opened with the parent row still
        // selected, so the selected row is not the active target).
        if self.active_worktree.is_some() {
            self.refresh_active_worktree();
            return;
        }
        if let Some(idx) = self.repo_list.selected_index()
            && let Some(entry) = self.repo_list.repos.get(idx)
        {
            let name = entry.name.clone();
            let repo_id = RepoId(entry.path.clone());
            let files = entry
                .status
                .as_ref()
                .map(|s| s.files.clone())
                .unwrap_or_default();
            self.file_list.set_files(files, &name, repo_id);

            let path = entry.path.clone();
            self.git_graph.load_repo(path, &name);
        }
    }
    /// Replace the live theme on App and every component.
    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme.clone();
        self.repo_list.set_theme(theme.clone());
        self.file_list.set_theme(theme.clone());
        self.git_graph.set_theme(theme.clone());
        self.graph_context_menu.set_theme(theme.clone());
        self.graph_filter_picker.set_theme(theme.clone());
        self.github_panel.set_theme(theme.clone());
        self.confirm_dialog.set_theme(theme.clone());
        self.context_menu.set_theme(theme.clone());
        self.path_input.set_theme(theme.clone());
        self.status_bar.set_theme(theme.clone());
        self.theme_picker.set_theme(theme.clone());
        self.picker.set_theme(theme);
    }
    fn schedule_refresh(&mut self, id: &RepoId) {
        if self.pending_status.contains(id) {
            self.dirty_repos.insert(id.clone());
            tracing::debug!("skipping repo {}: already in-flight (marked dirty)", id);
            return;
        }

        let cooldown = Duration::from_millis(self.config.watch.refresh_cooldown_ms);
        let now = Instant::now();
        match refresh_decision(self.last_refresh.get(id).copied(), now, cooldown) {
            RefreshDecision::Now => {
                self.refresh_scheduled.remove(id);
                self.spawn_refresh_query(id.clone());
            }
            RefreshDecision::Later(wait) => {
                if self.refresh_scheduled.insert(id.clone()) {
                    let repo_id = id.clone();
                    let tx = self.action_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(wait).await;
                        let _ = tx.send(Action::RefreshRepoAfterCooldown(repo_id));
                    });
                }
            }
        }
    }
    pub async fn run(&mut self) -> Result<()> {
        let mut tui = Tui::new()?
            .mouse(true)
            .poll_local_interval(std::time::Duration::from_secs(
                self.config.watch.poll_local_secs,
            ))
            .poll_fetch_interval(std::time::Duration::from_secs(
                self.config.watch.poll_fetch_secs,
            ));
        tui.enter()?;

        // Register action handlers
        self.repo_list
            .register_action_handler(self.action_tx.clone())?;
        self.file_list
            .register_action_handler(self.action_tx.clone())?;
        self.git_graph
            .register_action_handler(self.action_tx.clone())?;
        self.context_menu
            .register_action_handler(self.action_tx.clone())?;
        self.theme_picker
            .register_action_handler(self.action_tx.clone());

        // Init components
        self.repo_list.init()?;

        // Trigger immediate status poll so repos don't show "..." until the
        // first PollLocal timer fires. Goes through the semaphore-controlled path.
        self.action_tx.send(Action::PollLocal)?;

        // Start filesystem watcher. Stored on `self` so `Action::RescanRepos`
        // and `Action::DiscoverNewRepos` can rebuild it when the repo set
        // changes (otherwise newly-discovered repos would go unwatched).
        self.tui_event_tx = Some(tui.event_tx.clone());
        self.rebuild_watcher();

        // Check for updates in the background
        if self.config.ui.check_for_updates {
            let tx = self.action_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(version) = crate::update_checker::check_latest() {
                    let _ = tx.send(Action::UpdateAvailable(version));
                }
            });
        }

        // Warn once if the `git` binary is missing. gitpane reads repo state
        // via libgit2, so it still runs, but fetch/pull/submodule/diff actions
        // shell out to `git` and would otherwise fail with a cryptic OS error.
        if !crate::git::git_available() {
            self.action_tx.send(Action::Error(
                "git not found on PATH: viewing works, but fetch/pull and submodule actions need git"
                    .to_string(),
            ))?;
        }

        // Auto-select the first repo (graph loads once status arrives)
        self.sync_selection();

        loop {
            // Process events from TUI
            if let Some(event) = tui.event_rx.recv().await {
                match event {
                    Event::Quit => {
                        self.action_tx.send(Action::Quit)?;
                    }
                    Event::Tick => {
                        let has_pending_actions = !self.action_rx.is_empty();
                        self.action_tx.send(Action::Tick)?;
                        if has_pending_actions {
                            self.action_tx.send(Action::Render)?;
                        }
                    }
                    Event::Render => {
                        self.action_tx.send(Action::Render)?;
                    }
                    Event::Key(key) => {
                        self.handle_key_event(key)?;
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse_event(mouse)?;
                    }
                    Event::Paste(ref text) => {
                        // Bracketed paste only targets the text input overlay.
                        if self.path_input.visible {
                            self.path_input.paste(text);
                            self.action_tx.send(Action::Render)?;
                        }
                    }
                    Event::Resize(w, h) => {
                        self.action_tx.send(Action::Resize(w, h))?;
                    }
                    Event::RepoChanged(ref path) => {
                        self.action_tx
                            .send(Action::RefreshRepo(RepoId(path.clone())))?;
                    }
                    Event::ReposRootChanged => {
                        // Leading-edge: fire immediately if outside cooldown.
                        // Trailing-edge: if events arrive during cooldown,
                        // schedule one deferred fire so we still pick up
                        // repos that finished cloning mid-burst.
                        let cooldown = std::time::Duration::from_secs(
                            self.config.watch.discovery_cooldown_secs,
                        );
                        let now = Instant::now();
                        let elapsed = self.last_discovery.map(|t| now.duration_since(t));
                        let in_cooldown = elapsed.is_some_and(|d| d < cooldown);
                        if !in_cooldown {
                            self.last_discovery = Some(now);
                            self.discovery_pending = false;
                            self.action_tx.send(Action::DiscoverNewRepos)?;
                        } else if !self.discovery_pending {
                            self.discovery_pending = true;
                            let wait = cooldown.saturating_sub(elapsed.unwrap_or_default());
                            let tx = self.action_tx.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(wait).await;
                                let _ = tx.send(Action::DiscoverNewRepos);
                            });
                        }
                    }
                    Event::PollLocal => {
                        self.action_tx.send(Action::PollLocal)?;
                    }
                    Event::PollFetch => {
                        self.action_tx.send(Action::PollFetch)?;
                    }
                    Event::FocusGained => {
                        if let Some(entry) = self.repo_list.selected_repo() {
                            self.action_tx
                                .send(Action::RefreshRepo(RepoId(entry.path.clone())))?;
                        }
                    }
                    _ => {}
                }
            }

            // Process actions
            while let Ok(action) = self.action_rx.try_recv() {
                self.handle_action(action, &mut tui)?;
            }

            if self.should_quit {
                tui.exit()?;
                break;
            }
        }
        Ok(())
    }
}

/// Simple base64 encoder for OSC 52 clipboard
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[(n >> 18 & 0x3f) as usize] as char);
        result.push(CHARS[(n >> 12 & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[(n >> 6 & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
