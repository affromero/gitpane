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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusPanel {
    Repos,
    Changes,
    Graph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortOrder {
    Alphabetical,
    DirtyFirst,
}

impl SortOrder {
    fn next(self) -> Self {
        match self {
            Self::Alphabetical => Self::DirtyFirst,
            Self::DirtyFirst => Self::Alphabetical,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Alphabetical => "A-Z",
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
    error_message: Option<(String, Instant)>,
    success_message: Option<(String, Instant)>,
    /// Which border is being dragged: 0 = repos|changes, 1 = changes|graph
    dragging_border: Option<u8>,
    /// Fraction of the layout axis for each border (0.0..1.0).
    /// Index 0 is the repos/changes split. Index 1 is the changes/graph split.
    /// Applies to width in horizontal mode, height in vertical mode.
    border_frac: [f64; 2],
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
    /// "Go to session": the value is the chosen tmux session name.
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
        };

        let update_position = config.ui.update_position;
        let poll_semaphore = Arc::new(tokio::sync::Semaphore::new(
            config.watch.max_concurrent_polls,
        ));

        Self {
            config,
            should_quit: false,
            repo_list: RepoList::new(repo_paths, theme.clone()),
            file_list: FileList::new(theme.clone()),
            git_graph,
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
            error_message: None,
            success_message: None,
            dragging_border: None,
            border_frac: [0.25, 0.50],
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
        }
    }

    fn sort_repos(&mut self) {
        match self.sort_order {
            SortOrder::Alphabetical => {
                self.repo_list.repos.sort_by_key(|r| r.name.to_lowercase());
            }
            SortOrder::DirtyFirst => {
                self.repo_list.repos.sort_by(|a, b| {
                    let a_dirty = a.status.as_ref().map(|s| s.is_dirty).unwrap_or(false);
                    let b_dirty = b.status.as_ref().map(|s| s.is_dirty).unwrap_or(false);
                    b_dirty
                        .cmp(&a_dirty)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
        }
        // Reset selection to first
        if !self.repo_list.repos.is_empty() {
            self.repo_list.select_repo_row(0);
        }
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

    /// Run `git -C <repo_path> <args>` off-thread, marking the repo row busy and
    /// refreshing it on completion (via `GitOpComplete`). Used for worktree
    /// add/remove; mirrors the push/pull op flow.
    fn spawn_repo_git_op(&mut self, repo_path: std::path::PathBuf, args: Vec<String>) {
        let repo_id = RepoId(repo_path.clone());
        if let Some(idx) = self.repo_list.resolve_index(&repo_id) {
            self.repo_list.repos[idx].git_op = true;
        }
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let guard = GitOpGuard::new(repo_id.clone(), tx.clone());
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path)
                .args(&args)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    guard.complete();
                    let _ = tx.send(Action::GitOpComplete {
                        id: repo_id,
                        message: format!("git {} succeeded", args.join(" ")),
                    });
                }
                Ok(o) => {
                    guard.complete();
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let first_line = stderr
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("unknown error")
                        .trim();
                    let _ = tx.send(Action::Error(format!(
                        "git {} failed: {}",
                        args.join(" "),
                        first_line
                    )));
                    let _ = tx.send(Action::RefreshRepo(repo_id));
                }
                Err(e) => {
                    guard.complete();
                    let _ = tx.send(Action::Error(format!(
                        "git {} failed: {}",
                        args.join(" "),
                        crate::git::describe_spawn_error(&e)
                    )));
                    let _ = tx.send(Action::RefreshRepo(repo_id));
                }
            }
        });
    }

    /// Execute a [`crate::launcher::LaunchPlan`] for a verb (`open`/`review`)
    /// targeting `dir`. Needs `tui` so an `Inline` plan can suspend the TUI,
    /// run the command in the inherited terminal, and restore. Ask arrives in L3.
    fn run_launch_plan(
        &mut self,
        plan: crate::launcher::LaunchPlan,
        dir: std::path::PathBuf,
        label: &'static str,
        tui: &mut Tui,
    ) -> Result<()> {
        use crate::launcher::LaunchPlan;
        match plan {
            LaunchPlan::Spawn(argv) => {
                spawn_detached(argv, dir, self.action_tx.clone(), label);
            }
            LaunchPlan::Inline(cmd) => {
                // Suspend the TUI, run the command in the inherited terminal so
                // an interactive viewer/editor works, then restore. `enter()` is
                // called unconditionally — a failed command must not leave us
                // suspended — and `clear()` forces a full repaint of the
                // re-entered alternate screen.
                tui.exit()?;
                let status = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&dir)
                    .status();
                tui.enter()?;
                tui.terminal.clear()?;
                // Set the error first, then render, so a spawn failure's toast
                // is painted on the repaint rather than waiting for a later one.
                if let Err(e) = status {
                    self.action_tx.send(Action::Error(format!(
                        "{label} failed: {}",
                        crate::git::describe_spawn_error(&e)
                    )))?;
                }
                self.action_tx.send(Action::Render)?;
            }
            LaunchPlan::Ask => {
                self.action_tx
                    .send(Action::Error("ask placement isn't available yet".into()))?;
            }
            LaunchPlan::Error(msg) => {
                self.action_tx.send(Action::Error(msg))?;
            }
        }
        Ok(())
    }

    /// Park a launch and open the placement picker (`placement = "ask"`), listing
    /// the current tmux windows as right-of/below targets.
    fn start_placement_picker(
        &mut self,
        dir: std::path::PathBuf,
        command: Option<String>,
        base: Option<String>,
        label: &'static str,
    ) {
        let choices = crate::launcher::placement_choices(&crate::launcher::tmux_windows());
        self.pending_pick = Some(PendingPick::Launch(PendingLaunch {
            dir,
            command,
            base,
            label,
        }));
        self.picker.show("Open where?", choices);
    }

    /// Go to a repo/worktree's live tmux session(s): one goes directly via the
    /// `[goto] command`, several open the picker.
    fn goto_session_selected(&mut self) -> Result<()> {
        let path = self
            .repo_list
            .selected_worktree()
            .map(|(_, wt)| wt.path.clone())
            .or_else(|| self.repo_list.selected_repo().map(|e| e.path.clone()));
        let Some(path) = path else { return Ok(()) };
        let sessions = crate::liveness::live_sessions(&path, self.repo_list.live_panes());
        match sessions.as_slice() {
            [] => {
                self.action_tx
                    .send(Action::Error("no live tmux session here".into()))?;
            }
            [one] => self.goto_session(one),
            many => {
                let choices = many.iter().map(|s| (s.clone(), s.clone())).collect();
                self.pending_pick = Some(PendingPick::GotoSession);
                self.picker.show("Go to session", choices);
            }
        }
        Ok(())
    }

    /// Run the `[goto] command` for `session`. The command returns promptly
    /// (switches the tmux client or spawns a terminal tab), so its exit status
    /// is checked and a failure (e.g. a stale session) is surfaced.
    fn goto_session(&mut self, session: &str) {
        let argv = crate::launcher::build_goto_argv(&self.config.goto.command, session);
        if argv.is_empty() {
            return;
        }
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output();
            match output {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let first = stderr
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("command failed")
                        .trim();
                    let _ = tx.send(Action::Error(format!("goto failed: {first}")));
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!(
                        "goto failed: {}",
                        crate::git::describe_spawn_error(&e)
                    )));
                }
            }
        });
    }

    fn spawn_refresh_query(&mut self, repo_id: RepoId) {
        let sub_cfg = self.config.submodules.clone();
        if let Some(idx) = self.repo_list.resolve_index(&repo_id) {
            self.pending_status.insert(repo_id.clone());
            let path = self.repo_list.repos[idx].path.clone();
            let tx = self.action_tx.clone();
            let sem = self.poll_semaphore.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                tokio::task::spawn_blocking(move || {
                    match crate::git::status::query_status(&path, &sub_cfg) {
                        Ok(s) => {
                            let _ = tx.send(Action::RepoStatusUpdated {
                                id: repo_id.clone(),
                                status: s,
                            });
                            guard.complete();
                        }
                        Err(e) => {
                            guard.complete();
                            let _ = tx.send(Action::StatusQueryDone(repo_id.clone()));
                            // The watcher fires `RefreshRepo` on any change inside the repo,
                            // including `rm -rf <repo>` or `rm -rf <repo>/.git`. In that
                            // case the query naturally fails; surface a rescan instead of an
                            // error toast so the repo just disappears from the list.
                            if !path.join(".git").exists() {
                                tracing::debug!(
                                    "repo {} no longer a git repo; rescanning",
                                    path.display()
                                );
                                let _ = tx.send(Action::DiscoverNewRepos);
                            } else {
                                let _ = tx.send(Action::Error(format!("Failed to query: {}", e)));
                            }
                        }
                    }
                })
                .await
            });
        }
    }

    /// If a worktree is the active selection, refresh its graph and re-query
    /// its files so the changes panel updates live. Used by the local poll and
    /// after a git op completes on the worktree's parent repo (the parent
    /// status query updates the list row's ahead/behind, but the changes panel
    /// for a worktree is fed only by `WorktreeFilesLoaded`).
    fn refresh_active_worktree(&mut self) {
        let Some(aw) = self.active_worktree.clone() else {
            return;
        };
        // Refresh the graph from the worktree so new commits appear live.
        if self.git_graph.has_detail() {
            self.git_graph.set_needs_reload();
        } else {
            self.git_graph.load_repo(aw.path.clone(), &aw.display_name);
        }

        let tx = self.action_tx.clone();
        let sub_cfg = self.config.submodules.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(s) = crate::git::status::query_status(&aw.path, &sub_cfg) {
                let _ = tx.send(Action::WorktreeFilesLoaded {
                    repo_id: aw.repo_id,
                    worktree_path: aw.path,
                    name: aw.display_name,
                    files: s.files,
                });
            }
        });
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
                match action {
                    Action::Quit => {
                        self.should_quit = true;
                    }
                    Action::Tick => {
                        if self.clear_expired_messages() {
                            self.action_tx.send(Action::Render)?;
                        }
                    }
                    Action::Render => {
                        tui.terminal.draw(|frame| {
                            let _ = self.draw(frame);
                        })?;
                    }
                    Action::Resize(w, h) => {
                        tui.terminal
                            .resize(ratatui::layout::Rect::new(0, 0, w, h))?;
                    }
                    Action::SelectRepo(ref id) => {
                        self.context_menu.hide();
                        self.active_worktree = None;
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &self.repo_list.repos[idx];
                            let name = entry.name.clone();
                            let path = entry.path.clone();
                            let repo_id = id.clone();
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name, repo_id);
                            self.git_graph.load_repo(path, &name);
                            self.repo_list.select_repo_row(idx);
                        }
                    }
                    Action::FocusRepoDetails(ref id) => {
                        // Same panel refresh as SelectRepo but without
                        // moving the list selection — used by child rows
                        // (currently stash entries) so the cursor can
                        // remain on the child while the details panels
                        // re-target onto that child's parent repo.
                        self.context_menu.hide();
                        self.active_worktree = None;
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &self.repo_list.repos[idx];
                            let name = entry.name.clone();
                            let path = entry.path.clone();
                            let repo_id = id.clone();
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name, repo_id);
                            self.git_graph.load_repo(path, &name);
                        }
                    }
                    Action::SelectWorktree {
                        ref repo_id,
                        ref worktree_path,
                        ref worktree_branch,
                    } => {
                        self.context_menu.hide();

                        let repo_name = self
                            .repo_list
                            .resolve_index(repo_id)
                            .map(|i| self.repo_list.repos[i].name.clone())
                            .unwrap_or_default();
                        let display_name = format!("{}:{}", repo_name, worktree_branch);

                        self.active_worktree = Some(ActiveWorktree {
                            path: worktree_path.clone(),
                            repo_id: repo_id.clone(),
                            display_name: display_name.clone(),
                        });

                        // Clear file list while loading (use parent repo_id for resolve_index)
                        self.file_list
                            .set_files(Vec::new(), &display_name, repo_id.clone());

                        // Load graph from worktree path
                        self.git_graph
                            .load_repo(worktree_path.clone(), &display_name);

                        // Query worktree status in background
                        let wt_path = worktree_path.clone();
                        let parent_id = repo_id.clone();
                        let name = display_name;
                        let tx = self.action_tx.clone();
                        let sub_cfg = self.config.submodules.clone();
                        tokio::task::spawn_blocking(
                            move || match crate::git::status::query_status(&wt_path, &sub_cfg) {
                                Ok(s) => {
                                    let _ = tx.send(Action::WorktreeFilesLoaded {
                                        repo_id: parent_id,
                                        worktree_path: wt_path,
                                        name,
                                        files: s.files,
                                    });
                                }
                                Err(e) => {
                                    let _ =
                                        tx.send(Action::Error(format!("Worktree status: {}", e)));
                                }
                            },
                        );
                    }
                    Action::WorktreeFilesLoaded {
                        ref repo_id,
                        ref worktree_path,
                        ref name,
                        ref files,
                    } => {
                        // Only apply if this worktree is still selected
                        if self
                            .active_worktree
                            .as_ref()
                            .is_some_and(|aw| aw.path == *worktree_path)
                        {
                            self.file_list
                                .set_files(files.clone(), name, repo_id.clone());
                        }
                    }
                    Action::StatusQueryDone(ref id) => {
                        self.pending_status.remove(id);
                        self.last_refresh.insert(id.clone(), Instant::now());
                        // Clear git_op so the repo isn't permanently skipped
                        // by future polls after a failed status query.
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            self.repo_list.repos[idx].git_op = false;
                        }
                        if self.dirty_repos.remove(id) {
                            self.schedule_refresh(id);
                        }
                    }
                    Action::RepoStatusUpdated { ref id, ref status } => {
                        self.pending_status.remove(id);
                        self.last_refresh.insert(id.clone(), Instant::now());
                        let is_dirty = self.dirty_repos.remove(id);
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let graph_changed = graph_status_changed(
                                self.repo_list.repos[idx].status.as_ref(),
                                status,
                                self.git_graph.graph_options.branch_filter,
                            );
                            let status_clone = status.clone();
                            self.repo_list.update_status(idx, status_clone);

                            // Refresh the file list so stale diffs are cleared
                            // when files are staged/unstaged. Skip when a worktree
                            // is being viewed — its files come from WorktreeFilesLoaded,
                            // not the parent repo's status.
                            if self.repo_list.selected_index() == Some(idx)
                                && self.active_worktree.is_none()
                            {
                                let entry = &self.repo_list.repos[idx];
                                let name = entry.name.clone();
                                let repo_id = id.clone();
                                let files = entry
                                    .status
                                    .as_ref()
                                    .map(|s| s.files.clone())
                                    .unwrap_or_default();
                                self.file_list.set_files(files, &name, repo_id);

                                if graph_changed {
                                    if self.git_graph.has_detail() {
                                        self.git_graph.set_needs_reload();
                                    } else {
                                        let path = entry.path.clone();
                                        self.git_graph.load_repo(path, &name);
                                    }
                                }
                            }
                        }
                        if is_dirty {
                            self.schedule_refresh(id);
                        }
                    }
                    Action::RefreshAll => {
                        // User-initiated refresh: fetch from remote + show spinner
                        let sub_cfg = self.config.submodules.clone();
                        for entry in self.repo_list.repos.iter_mut() {
                            entry.git_op = true;
                            let repo_id = RepoId(entry.path.clone());
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            let sub_cfg = sub_cfg.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status_with_fetch(
                                        &path, &sub_cfg,
                                    ) {
                                        Ok(s) => {
                                            let _ = tx.send(Action::RepoStatusUpdated {
                                                id: repo_id.clone(),
                                                status: s,
                                            });
                                            guard.complete();
                                        }
                                        Err(e) => {
                                            guard.complete();
                                            let _ = tx.send(Action::StatusQueryDone(repo_id));
                                            let _ = tx.send(Action::Error(format!(
                                                "Failed to query: {}",
                                                e
                                            )));
                                        }
                                    }
                                })
                                .await
                            });
                        }
                    }
                    Action::PollLocal => {
                        // Probe tmux pane cwds once per poll so live
                        // repos/worktrees get a marker (tmux-only; empty set
                        // otherwise). One `tmux list-panes` call, off-thread.
                        if self.config.ui.show_liveness && !self.liveness_probe_in_flight {
                            self.liveness_probe_in_flight = true;
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                let _ = tx.send(Action::LiveSessionsLoaded(
                                    crate::liveness::tmux_pane_sessions(),
                                ));
                            });
                        }
                        // Fast local status poll (no network, no spinner)
                        let sub_cfg = self.config.submodules.clone();
                        for entry in self.repo_list.repos.iter() {
                            let repo_id = RepoId(entry.path.clone());
                            if entry.git_op || self.pending_status.contains(&repo_id) {
                                continue;
                            }
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            let sub_cfg = sub_cfg.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status(&path, &sub_cfg) {
                                        Ok(s) => {
                                            let _ = tx.send(Action::RepoStatusUpdated {
                                                id: repo_id.clone(),
                                                status: s,
                                            });
                                            guard.complete();
                                        }
                                        Err(e) => {
                                            guard.complete();
                                            let _ = tx.send(Action::StatusQueryDone(repo_id));
                                            tracing::debug!(
                                                "Local poll failed for {}: {}",
                                                path.display(),
                                                e
                                            );
                                        }
                                    }
                                })
                                .await
                            });
                        }

                        // Also re-query the active worktree so its changes update live
                        self.refresh_active_worktree();
                    }
                    Action::PollFetch => {
                        // Remote fetch poll (updates ahead/behind, no spinner)
                        let sub_cfg = self.config.submodules.clone();
                        for entry in self.repo_list.repos.iter() {
                            let repo_id = RepoId(entry.path.clone());
                            if entry.git_op || self.pending_status.contains(&repo_id) {
                                continue;
                            }
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            let sub_cfg = sub_cfg.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status_with_fetch(
                                        &path, &sub_cfg,
                                    ) {
                                        Ok(s) => {
                                            let _ = tx.send(Action::RepoStatusUpdated {
                                                id: repo_id.clone(),
                                                status: s,
                                            });
                                            guard.complete();
                                        }
                                        Err(e) => {
                                            guard.complete();
                                            let _ = tx.send(Action::StatusQueryDone(repo_id));
                                            tracing::debug!(
                                                "Fetch poll failed for {}: {}",
                                                path.display(),
                                                e
                                            );
                                        }
                                    }
                                })
                                .await
                            });
                        }
                    }
                    Action::RefreshRepo(ref id) => {
                        // Watcher-triggered: fast local-only, no spinner. A
                        // worktree path resolves to its parent repo, whose
                        // status query re-reads worktree state.
                        let parent_id = self
                            .repo_list
                            .resolve_target(id)
                            .map(|t| RepoId(self.repo_list.repos[t.parent_index].path.clone()));
                        self.schedule_refresh(parent_id.as_ref().unwrap_or(id));
                    }
                    Action::RefreshRepoAfterCooldown(ref id) => {
                        if self.refresh_scheduled.remove(id) {
                            self.schedule_refresh(id);
                        }
                    }
                    Action::ShowGitGraph => {
                        // Force-reload the graph for the current selection. A
                        // worktree row routes through SelectWorktree so the
                        // graph and changes panel target the worktree, matching
                        // the other context-menu items; a repo row loads the
                        // repo's own graph.
                        self.context_menu.hide();
                        let worktree = self
                            .repo_list
                            .selected_worktree()
                            .map(|(repo_id, wt)| (repo_id, wt.path.clone(), wt.branch.clone()));
                        if let Some((repo_id, worktree_path, worktree_branch)) = worktree {
                            self.action_tx.send(Action::SelectWorktree {
                                repo_id,
                                worktree_path,
                                worktree_branch,
                            })?;
                        } else if let Some(entry) = self.repo_list.selected_repo() {
                            let path = entry.path.clone();
                            let name = entry.name.clone();
                            self.git_graph.load_repo(path, &name);
                        }
                        self.focus = FocusPanel::Graph;
                    }
                    Action::ShowFileList => {
                        self.focus = FocusPanel::Changes;
                    }
                    Action::OpenSelected => {
                        // Resolve the highlighted target's directory: a worktree
                        // row opens the worktree's own path, otherwise the
                        // selected repo's path. Mirrors ShowGitGraph resolution.
                        let path = self
                            .repo_list
                            .selected_worktree()
                            .map(|(_, wt)| wt.path.clone())
                            .or_else(|| self.repo_list.selected_repo().map(|e| e.path.clone()));
                        if let Some(path) = path {
                            let command = self.config.open.command.clone();
                            let plan = crate::launcher::plan(
                                command.as_deref(),
                                &self.config.open.placement,
                                &path.to_string_lossy(),
                                None,
                                std::env::var_os("TMUX").is_some(),
                            );
                            if matches!(plan, crate::launcher::LaunchPlan::Ask) {
                                self.start_placement_picker(path, command, None, "open");
                            } else {
                                self.run_launch_plan(plan, path, "open", &mut tui)?;
                            }
                        }
                    }
                    Action::ReviewSelected => {
                        // Review the highlighted repo/worktree's diff vs its base
                        // branch via the [review] launcher. Same selection
                        // resolution as OpenSelected.
                        let path = self
                            .repo_list
                            .selected_worktree()
                            .map(|(_, wt)| wt.path.clone())
                            .or_else(|| self.repo_list.selected_repo().map(|e| e.path.clone()));
                        if let Some(path) = path {
                            // Base ref: explicit `[review] base`, else the repo's
                            // resolved default branch. No silent fallback — a
                            // doomed `git diff origin/HEAD...HEAD` is worse than a
                            // clear in-app error.
                            let base = self.config.review.base.clone().or_else(|| {
                                git2::Repository::open(&path)
                                    .ok()
                                    .and_then(|r| crate::git::status::default_branch_name(&r))
                            });
                            if let Some(base) = base {
                                let command = self
                                    .config
                                    .review
                                    .command
                                    .clone()
                                    .unwrap_or_else(|| "git diff {base}...HEAD".to_string());
                                let plan = crate::launcher::plan(
                                    Some(&command),
                                    &self.config.review.placement,
                                    &path.to_string_lossy(),
                                    Some(&base),
                                    std::env::var_os("TMUX").is_some(),
                                );
                                if matches!(plan, crate::launcher::LaunchPlan::Ask) {
                                    self.start_placement_picker(
                                        path,
                                        Some(command),
                                        Some(base),
                                        "review",
                                    );
                                } else {
                                    self.run_launch_plan(plan, path, "review", &mut tui)?;
                                }
                            } else {
                                self.action_tx.send(Action::Error(
                                    "no base branch resolved; set [review] base in config".into(),
                                ))?;
                            }
                        }
                    }
                    Action::OpenNewWorktree(ref repo_id) => {
                        self.path_input.show_new_worktree(repo_id.clone());
                    }
                    Action::CreateWorktree {
                        ref repo,
                        ref branch,
                    } => {
                        // A leading '-' would be read as an option by git; reject
                        // it up front with a clear message rather than a cryptic
                        // git error.
                        if branch.starts_with('-') {
                            self.action_tx
                                .send(Action::Error("branch name cannot start with '-'".into()))?;
                        } else {
                            let new_path =
                                crate::config::worktree_path(&self.config.worktree, repo, branch);
                            // `-b <branch>` then `--` so the path is never parsed
                            // as an option.
                            let args = vec![
                                "worktree".to_string(),
                                "add".to_string(),
                                "-b".to_string(),
                                branch.clone(),
                                "--".to_string(),
                                new_path.to_string_lossy().to_string(),
                            ];
                            self.spawn_repo_git_op(repo.clone(), args);
                        }
                    }
                    Action::RemoveWorktreeSelected => {
                        // Resolve to the parent repo path + worktree path, then
                        // confirm before removing.
                        let wt = self
                            .repo_list
                            .selected_worktree()
                            .map(|(rid, wt)| (rid.0.clone(), wt.path.clone(), wt.branch.clone()));
                        if let Some((repo, worktree_path, branch)) = wt {
                            self.confirm_dialog.show(
                                format!("Remove worktree '{branch}'?"),
                                Action::RemoveWorktree {
                                    repo,
                                    worktree_path,
                                },
                            );
                        }
                    }
                    Action::RemoveWorktree {
                        ref repo,
                        ref worktree_path,
                    } => {
                        // If we're removing the worktree whose changes/graph are
                        // currently shown, drop it so nothing refreshes against a
                        // now-deleted path.
                        if self
                            .active_worktree
                            .as_ref()
                            .is_some_and(|aw| &aw.path == worktree_path)
                        {
                            self.active_worktree = None;
                        }
                        let args = vec![
                            "worktree".to_string(),
                            "remove".to_string(),
                            "--".to_string(),
                            worktree_path.to_string_lossy().to_string(),
                        ];
                        self.spawn_repo_git_op(repo.clone(), args);
                    }
                    Action::LiveSessionsLoaded(panes) => {
                        self.liveness_probe_in_flight = false;
                        self.repo_list.set_live_panes(panes);
                    }
                    Action::PickerChose(ref value) => {
                        self.picker.hide();
                        match self.pending_pick.take() {
                            Some(PendingPick::Launch(p)) => {
                                let plan = crate::launcher::plan(
                                    p.command.as_deref(),
                                    value,
                                    &p.dir.to_string_lossy(),
                                    p.base.as_deref(),
                                    std::env::var_os("TMUX").is_some(),
                                );
                                self.run_launch_plan(plan, p.dir, p.label, &mut tui)?;
                            }
                            Some(PendingPick::GotoSession) => self.goto_session(value),
                            None => {}
                        }
                    }
                    Action::PickerCancel => {
                        self.picker.hide();
                        self.pending_pick = None;
                    }
                    Action::GotoSessionSelected => {
                        self.goto_session_selected()?;
                    }
                    Action::GraphLoaded { generation, rows } => {
                        if generation == self.git_graph.current_generation() {
                            self.git_graph.set_rows(rows);
                        }
                    }
                    Action::DiffStatsLoaded { generation, stats } => {
                        if generation == self.git_graph.current_generation() {
                            self.git_graph.set_diff_stats(stats);
                        }
                    }
                    Action::GraphError {
                        generation,
                        ref message,
                    } => {
                        // Drop stale errors so a failed build from a previous
                        // repo/generation can't clobber the current graph.
                        if generation == self.git_graph.current_generation() {
                            self.git_graph.set_error(message.clone());
                        }
                    }
                    Action::GraphLoadAborted { generation } => {
                        if generation == self.git_graph.current_generation() {
                            self.git_graph.abort_load();
                        }
                    }
                    Action::ShowContextMenu {
                        ref id,
                        row,
                        col,
                        is_worktree,
                    } => {
                        if let Some(target) = self.repo_list.resolve_target(id) {
                            self.context_menu.show(
                                id.clone(),
                                col,
                                row,
                                crate::components::context_menu::MenuContext {
                                    ahead: target.ahead,
                                    behind: target.behind,
                                    has_submodules: target.has_submodules,
                                    is_worktree,
                                },
                            );
                        }
                    }
                    Action::HideContextMenu => {
                        self.context_menu.hide();
                    }
                    Action::CopyPath(ref id) => {
                        if let Some(target) = self.repo_list.resolve_target(id) {
                            let path_str = target.exec_path.to_string_lossy().to_string();
                            use std::io::Write;
                            let encoded = base64_encode(path_str.as_bytes());
                            let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Action::GitPush(ref id)
                    | Action::GitPull(ref id)
                    | Action::GitPullRebase(ref id)
                    | Action::GitPullSubmodules(ref id) => {
                        if let Some(target) = self.repo_list.resolve_target(id) {
                            let branch = target.branch.clone();
                            let mut git_args: Vec<String> = match action {
                                Action::GitPush(_) => vec!["push".into()],
                                Action::GitPull(_) => vec!["pull".into()],
                                Action::GitPullRebase(_) => {
                                    vec!["pull".into(), "--rebase".into()]
                                }
                                Action::GitPullSubmodules(_) => {
                                    vec!["pull".into(), "--recurse-submodules".into()]
                                }
                                _ => unreachable!(),
                            };
                            // Add origin <branch> so pull/push works even without upstream config
                            if !branch.is_empty() && branch != "(no branch)" {
                                git_args.push("origin".into());
                                git_args.push(branch);
                            }
                            // The op runs in `exec_path` (the worktree's own
                            // directory when a worktree is targeted) but the
                            // parent repo's row shows the spinner and is
                            // refreshed afterward.
                            let parent_id =
                                RepoId(self.repo_list.repos[target.parent_index].path.clone());
                            self.repo_list.repos[target.parent_index].git_op = true;
                            let path = target.exec_path.clone();
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                let guard = GitOpGuard::new(parent_id.clone(), tx.clone());
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&path)
                                    .args(&git_args)
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        guard.complete();
                                        let _ = tx.send(Action::GitOpComplete {
                                            id: parent_id,
                                            message: format!(
                                                "git {} succeeded",
                                                git_args.join(" ")
                                            ),
                                        });
                                    }
                                    Ok(o) => {
                                        guard.complete();
                                        let stderr = String::from_utf8_lossy(&o.stderr);
                                        let first_line = stderr
                                            .lines()
                                            .find(|l| !l.trim().is_empty())
                                            .unwrap_or("unknown error")
                                            .trim();
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            git_args.join(" "),
                                            first_line
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(parent_id));
                                    }
                                    Err(e) => {
                                        guard.complete();
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            git_args.join(" "),
                                            crate::git::describe_spawn_error(&e)
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(parent_id));
                                    }
                                }
                            });
                        }
                    }
                    Action::GitSubmoduleUpdate(ref id)
                    | Action::GitSubmoduleSync(ref id)
                    | Action::GitSubmoduleUpdateLatest(ref id) => {
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &mut self.repo_list.repos[idx];
                            let git_args: Vec<String> = match action {
                                Action::GitSubmoduleUpdate(_) => {
                                    ["submodule", "update", "--init", "--recursive"]
                                        .iter()
                                        .map(|s| s.to_string())
                                        .collect()
                                }
                                Action::GitSubmoduleSync(_) => ["submodule", "sync"]
                                    .iter()
                                    .map(|s| s.to_string())
                                    .collect(),
                                Action::GitSubmoduleUpdateLatest(_) => {
                                    ["submodule", "foreach", "git", "pull", "origin", "HEAD"]
                                        .iter()
                                        .map(|s| s.to_string())
                                        .collect()
                                }
                                _ => unreachable!(),
                            };
                            entry.git_op = true;
                            let path = entry.path.clone();
                            let repo_id = id.clone();
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                let guard = GitOpGuard::new(repo_id.clone(), tx.clone());
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&path)
                                    .args(&git_args)
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        guard.complete();
                                        let _ = tx.send(Action::GitOpComplete {
                                            id: repo_id,
                                            message: format!(
                                                "git {} succeeded",
                                                git_args.join(" ")
                                            ),
                                        });
                                    }
                                    Ok(o) => {
                                        guard.complete();
                                        let stderr = String::from_utf8_lossy(&o.stderr);
                                        let first_line = stderr
                                            .lines()
                                            .find(|l| !l.trim().is_empty())
                                            .unwrap_or("unknown error")
                                            .trim();
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            git_args.join(" "),
                                            first_line
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(repo_id));
                                    }
                                    Err(e) => {
                                        guard.complete();
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            git_args.join(" "),
                                            crate::git::describe_spawn_error(&e)
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(repo_id));
                                    }
                                }
                            });
                        }
                    }
                    Action::GitOpComplete {
                        ref id,
                        ref message,
                    } => {
                        self.success_message = Some((message.clone(), Instant::now()));
                        self.action_tx.send(Action::RefreshRepo(id.clone()))?;
                        // The parent refresh updates the list row's ahead/behind.
                        // A worktree's changes panel is fed only by
                        // WorktreeFilesLoaded, so if a worktree of this repo is
                        // the active selection, refresh it now instead of
                        // waiting for the next local poll.
                        if self
                            .active_worktree
                            .as_ref()
                            .is_some_and(|aw| aw.repo_id == *id)
                        {
                            self.refresh_active_worktree();
                        }
                    }
                    Action::ShowDiff(ref id, ref file_path) => {
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &self.repo_list.repos[idx];
                            let diff_gen = self.file_list.diff_generation();
                            let sub_info = entry
                                .status
                                .as_ref()
                                .and_then(|s| s.submodules.iter().find(|sm| sm.path == *file_path));

                            if let Some(sub) = sub_info {
                                let repo_path = entry.path.clone();
                                let sub_path = file_path.clone();
                                let old_oid = sub.head_oid.clone().unwrap_or_default();
                                let new_oid = sub.workdir_oid.clone().unwrap_or_default();
                                let sub_state = sub.state.clone();
                                let tx = self.action_tx.clone();
                                tokio::task::spawn_blocking(move || {
                                    let submodule_abs = repo_path.join(&sub_path);
                                    let short_old = if old_oid.len() >= 7 {
                                        &old_oid[..7]
                                    } else {
                                        &old_oid
                                    };
                                    let short_new = if new_oid.len() >= 7 {
                                        &new_oid[..7]
                                    } else {
                                        &new_oid
                                    };

                                    // Dirty submodule (local uncommitted changes): show git diff
                                    // Modified submodule (pointer changed): show commit log
                                    let pointer_changed = !old_oid.is_empty()
                                        && !new_oid.is_empty()
                                        && old_oid != new_oid;
                                    let use_diff = sub_state
                                        == Some(crate::git::status::SubmoduleState::Dirty)
                                        || !pointer_changed;

                                    if use_diff {
                                        let label = match sub_state {
                                            Some(crate::git::status::SubmoduleState::Dirty) => {
                                                "uncommitted changes"
                                            }
                                            Some(
                                                crate::git::status::SubmoduleState::Uninitialized,
                                            ) => "not initialized",
                                            Some(crate::git::status::SubmoduleState::Modified) => {
                                                "modified"
                                            }
                                            None => "unpushed",
                                        };
                                        let header = format!(
                                            "Submodule {} ({})\n{}\n",
                                            sub_path.display(),
                                            label,
                                            "─".repeat(40),
                                        );
                                        let output = std::process::Command::new("git")
                                            .arg("-C")
                                            .arg(&submodule_abs)
                                            .args(["diff", "HEAD"])
                                            .output();
                                        let body = match output {
                                            Ok(o) => {
                                                let text =
                                                    String::from_utf8_lossy(&o.stdout).to_string();
                                                if text.is_empty() {
                                                    // Fallback: show status
                                                    let status_out =
                                                        std::process::Command::new("git")
                                                            .arg("-C")
                                                            .arg(&submodule_abs)
                                                            .args(["status", "--short"])
                                                            .output()
                                                            .map(|o| {
                                                                String::from_utf8_lossy(&o.stdout)
                                                                    .to_string()
                                                            })
                                                            .unwrap_or_default();
                                                    if status_out.is_empty() {
                                                        "(no changes detected)".to_string()
                                                    } else {
                                                        status_out
                                                    }
                                                } else {
                                                    text
                                                }
                                            }
                                            Err(e) => {
                                                format!(
                                                    "Failed to get submodule diff: {}",
                                                    crate::git::describe_spawn_error(&e)
                                                )
                                            }
                                        };
                                        let _ = tx.send(Action::DiffLoaded {
                                            generation: diff_gen,
                                            content: format!("{}{}", header, body),
                                        });
                                    } else {
                                        // Pointer changed: show commit log between old and new
                                        let header = format!(
                                            "Submodule {} → {}\n{}\n",
                                            short_old,
                                            short_new,
                                            "─".repeat(40),
                                        );
                                        let range = format!("{}..{}", old_oid, new_oid);
                                        let output = std::process::Command::new("git")
                                            .arg("-C")
                                            .arg(&submodule_abs)
                                            .args(["log", "--oneline", "--graph", &range])
                                            .output();
                                        let body = match output {
                                            Ok(o) => {
                                                let text =
                                                    String::from_utf8_lossy(&o.stdout).to_string();
                                                if text.is_empty() {
                                                    "(no commits in range)".to_string()
                                                } else {
                                                    text
                                                }
                                            }
                                            Err(e) => format!(
                                                "Failed to get submodule log: {}",
                                                crate::git::describe_spawn_error(&e)
                                            ),
                                        };
                                        let _ = tx.send(Action::DiffLoaded {
                                            generation: diff_gen,
                                            content: format!("{}{}", header, body),
                                        });
                                    }
                                });
                            } else {
                                // Use worktree path for diffs when a worktree is selected
                                let path = self
                                    .active_worktree
                                    .as_ref()
                                    .map(|aw| aw.path.clone())
                                    .unwrap_or_else(|| entry.path.clone());
                                let fp = file_path.clone();
                                let tx = self.action_tx.clone();
                                tokio::task::spawn_blocking(move || {
                                    let output = std::process::Command::new("git")
                                        .arg("-C")
                                        .arg(&path)
                                        .arg("diff")
                                        .arg("HEAD")
                                        .arg("--")
                                        .arg(&fp)
                                        .output();
                                    match output {
                                        Ok(o) => {
                                            let mut text =
                                                String::from_utf8_lossy(&o.stdout).to_string();
                                            if text.is_empty() {
                                                text = String::from_utf8_lossy(&{
                                                    std::process::Command::new("git")
                                                        .arg("-C")
                                                        .arg(&path)
                                                        .arg("diff")
                                                        .arg("--no-index")
                                                        .arg("/dev/null")
                                                        .arg(&fp)
                                                        .output()
                                                        .map(|o| o.stdout)
                                                        .unwrap_or_default()
                                                })
                                                .to_string();
                                            }
                                            if text.is_empty() {
                                                text = "(no diff available)".to_string();
                                            }
                                            let _ = tx.send(Action::DiffLoaded {
                                                generation: diff_gen,
                                                content: text,
                                            });
                                        }
                                        Err(e) => {
                                            let _ = tx.send(Action::DiffLoaded {
                                                generation: diff_gen,
                                                content: format!(
                                                    "Failed to get diff: {}",
                                                    crate::git::describe_spawn_error(&e)
                                                ),
                                            });
                                        }
                                    }
                                });
                            }
                        }
                    }
                    Action::DiffLoaded {
                        generation,
                        ref content,
                    } => {
                        if generation == self.file_list.diff_generation() {
                            self.file_list.set_diff(content.clone());
                        }
                    }
                    Action::ShowCommitFiles {
                        ref repo_path,
                        ref oid,
                    } => {
                        let detail_gen = self.git_graph.current_detail_generation();
                        let path = repo_path.clone();
                        let oid = oid.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            match crate::git::commit_files::list_commit_files(&path, &oid) {
                                Ok((message, files)) => {
                                    let _ = tx.send(Action::CommitFilesLoaded {
                                        generation: detail_gen,
                                        oid,
                                        message,
                                        files,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Error(format!(
                                        "Failed to list commit files: {}",
                                        e
                                    )));
                                }
                            }
                        });
                    }
                    Action::CommitFilesLoaded {
                        generation,
                        ref oid,
                        ref message,
                        ref files,
                    } => {
                        if generation == self.git_graph.current_detail_generation() {
                            self.git_graph.set_commit_files(
                                oid.clone(),
                                message.clone(),
                                files.clone(),
                            );
                        }
                    }
                    Action::ShowCommitDiff {
                        ref repo_path,
                        ref oid,
                        ref file_path,
                    } => {
                        let detail_gen = self.git_graph.current_detail_generation();
                        let path = repo_path.clone();
                        let oid = oid.clone();
                        let fp = file_path.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            match crate::git::commit_files::commit_file_diff(&path, &oid, &fp) {
                                Ok(diff) => {
                                    let _ = tx.send(Action::CommitDiffLoaded {
                                        generation: detail_gen,
                                        content: diff,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::Error(format!(
                                        "Failed to get commit diff: {}",
                                        e
                                    )));
                                }
                            }
                        });
                    }
                    Action::CommitDiffLoaded {
                        generation,
                        ref content,
                    } => {
                        if generation == self.git_graph.current_detail_generation() {
                            self.git_graph.set_commit_diff(content.clone());
                        }
                    }
                    Action::OpenAddRepo => {
                        self.path_input.show();
                    }
                    Action::AddRepo(ref path) => {
                        self.path_input.hide();
                        let path = path.clone();
                        if !path.join(".git").exists() && !path.join("HEAD").exists() {
                            tracing::error!("Not a git repository: {}", path.display());
                        } else {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.to_string_lossy().to_string());
                            self.config.add_pinned_repo(path.clone());
                            if let Err(e) = self.config.save() {
                                tracing::error!("Failed to save config: {}", e);
                            }
                            let repo_id = RepoId(path.clone());
                            self.repo_list.repos.push(RepoEntry {
                                path,
                                name,
                                status: None,
                                git_op: false,
                            });
                            self.action_tx.send(Action::RefreshRepo(repo_id.clone()))?;
                            self.action_tx.send(Action::SelectRepo(repo_id))?;
                        }
                    }
                    Action::RemoveRepo(ref id) => {
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            // Clean up tracking sets for the removed repo
                            self.pending_status.remove(id);
                            self.dirty_repos.remove(id);
                            self.last_refresh.remove(id);
                            self.refresh_scheduled.remove(id);
                            let entry = &self.repo_list.repos[idx];
                            // Remove from pinned if it was pinned
                            self.config.pinned_repos.retain(|p| *p != entry.path);
                            // Add to excluded so it won't reappear on rescan
                            let name = entry.name.clone();
                            if !self.config.excluded_repos.contains(&name) {
                                self.config.excluded_repos.push(name);
                            }
                            if let Err(e) = self.config.save() {
                                tracing::error!("Failed to save config: {}", e);
                            }
                            self.repo_list.repos.remove(idx);
                            // Fix selection
                            if self.repo_list.repos.is_empty() {
                                self.repo_list.state.select(None);
                                self.file_list.set_files(
                                    Vec::new(),
                                    "",
                                    RepoId(std::path::PathBuf::new()),
                                );
                            } else {
                                let new_idx = idx.min(self.repo_list.repos.len() - 1);
                                self.repo_list.select_repo_row(new_idx);
                                let new_id = RepoId(self.repo_list.repos[new_idx].path.clone());
                                self.action_tx.send(Action::SelectRepo(new_id))?;
                            }
                        }
                    }
                    Action::CycleSortOrder => {
                        self.sort_order = self.sort_order.next();
                        self.sort_repos();
                        self.sync_selection();
                    }
                    Action::RescanRepos => {
                        // Clear tracking sets — old paths are stale after rescan
                        self.pending_status.clear();
                        self.dirty_repos.clear();
                        self.last_refresh.clear();
                        self.refresh_scheduled.clear();
                        // Clear user-added exclusions, save, and re-discover repos
                        self.config.excluded_repos.clear();
                        if let Err(e) = self.config.save() {
                            tracing::error!("Failed to save config: {}", e);
                        }
                        let repo_paths = scanner::discover_repos(&self.config);
                        self.repo_list = RepoList::new(repo_paths, self.theme.clone());
                        self.repo_list
                            .register_action_handler(self.action_tx.clone())?;
                        self.repo_list.init()?;
                        self.rebuild_watcher();
                        self.action_tx.send(Action::PollLocal)?;
                        self.sort_repos();
                        self.sync_selection();
                    }
                    Action::DiscoverNewRepos => {
                        // Idempotent rescan: re-discover, but only mutate state
                        // when the set actually changed. Preserves selection,
                        // exclusions, in-flight queries, and dirty markers.
                        // Clear the pending-trailing-edge flag and refresh the
                        // cooldown anchor so back-to-back fires coalesce.
                        self.discovery_pending = false;
                        self.last_discovery = Some(Instant::now());
                        let new_paths = scanner::discover_repos(&self.config);
                        let saved_selection: Option<RepoId> = self
                            .repo_list
                            .selected_repo()
                            .map(|e| RepoId(e.path.clone()));
                        let diff = self.repo_list.sync_paths(new_paths);
                        if diff.is_empty() {
                            // No-op: discovered set matches the current list.
                        } else {
                            tracing::debug!(
                                "DiscoverNewRepos: +{} -{}",
                                diff.added.len(),
                                diff.removed.len()
                            );
                            // Re-register so newly-added rows hand mouse events back.
                            self.repo_list
                                .register_action_handler(self.action_tx.clone())?;
                            // Prune per-repo state for vanished repos so HashSets
                            // don't keep growing across the session.
                            for path in &diff.removed {
                                let id = RepoId(path.clone());
                                self.pending_status.remove(&id);
                                self.dirty_repos.remove(&id);
                                self.last_refresh.remove(&id);
                                self.refresh_scheduled.remove(&id);
                                if self
                                    .active_worktree
                                    .as_ref()
                                    .is_some_and(|aw| aw.repo_id == id)
                                {
                                    self.active_worktree = None;
                                }
                            }
                            self.sort_repos();
                            // Restore selection by saved repo id; fall back to
                            // first row if the previously-selected repo vanished.
                            if let Some(id) = saved_selection
                                && let Some(idx) = self.repo_list.resolve_index(&id)
                            {
                                self.repo_list.select_repo_row(idx);
                            }
                            self.rebuild_watcher();
                            // Queue status for newly added repos.
                            for path in &diff.added {
                                self.action_tx
                                    .send(Action::RefreshRepo(RepoId(path.clone())))?;
                            }
                            self.sync_selection();
                        }
                    }
                    Action::UpdateAvailable(ref version) => {
                        self.update_version = Some(version.clone());
                    }
                    Action::Error(ref msg) => {
                        tracing::error!("{}", msg);
                        // Sanitize: single line, max 120 chars for status bar
                        let clean: String = msg
                            .chars()
                            .map(|c| if c == '\n' { ' ' } else { c })
                            .collect();
                        let truncated = if clean.len() > 120 {
                            format!("{}...", &clean[..117])
                        } else {
                            clean
                        };
                        self.error_message = Some((truncated, Instant::now()));
                    }
                    Action::OpenThemePicker => {
                        let env = crate::config::RealEnv;
                        let dirs = self.config.theme_dirs(&env);
                        let themes = discover_all_theme_names(&dirs);
                        let current_name = self.config.effective_theme_name().to_string();
                        let current_theme = self.theme.clone();
                        self.theme_picker.show(themes, &current_name, current_theme);
                    }
                    Action::PreviewTheme(name) => {
                        let env = crate::config::RealEnv;
                        let dirs = self.config.theme_dirs(&env);
                        match load_theme(&name, &dirs) {
                            Ok(t) => {
                                self.apply_theme(Arc::new(t));
                                // Preview routes through the session
                                // override so unrelated saves do not pin
                                // the previewed name to disk.
                                self.config.runtime_theme_override = Some(name);
                            }
                            Err(e) => {
                                tracing::warn!("theme preview failed: {e}");
                            }
                        }
                    }
                    Action::CommitTheme(name) => {
                        let env = crate::config::RealEnv;
                        let dirs = self.config.theme_dirs(&env);
                        match load_theme(&name, &dirs) {
                            Ok(t) => {
                                self.apply_theme(Arc::new(t));
                                // Commit is explicit: drop the runtime
                                // override and promote the choice to the
                                // persisted field.
                                self.config.runtime_theme_override = None;
                                self.config.theme_name = name.clone();
                                if let Err(e) = self.config.save() {
                                    tracing::warn!("failed to persist theme: {e}");
                                    self.error_message =
                                        Some((format!("save failed: {e}"), Instant::now()));
                                } else {
                                    self.success_message =
                                        Some((format!("theme: {name}"), Instant::now()));
                                }
                            }
                            Err(e) => {
                                self.error_message =
                                    Some((format!("theme commit failed: {e}"), Instant::now()));
                            }
                        }
                        self.theme_picker.hide();
                    }
                    Action::CancelThemePreview => {
                        // Restore the captured Arc<Theme> snapshot byte-for-
                        // byte so cancel works even if the original theme
                        // name no longer loads.
                        if let Some(theme_snapshot) = self.theme_picker.original_theme() {
                            self.apply_theme(theme_snapshot);
                        }
                        // Re-establish the original override state. If the
                        // captured "current" matches the persisted name,
                        // there was no runtime override before the picker
                        // opened; otherwise a `--theme` override was active.
                        let original = self.theme_picker.original_name();
                        match original {
                            Some(name) if name == self.config.theme_name => {
                                self.config.runtime_theme_override = None;
                            }
                            Some(name) => {
                                self.config.runtime_theme_override = Some(name);
                            }
                            None => {
                                self.config.runtime_theme_override = None;
                            }
                        }
                        self.theme_picker.hide();
                    }
                    _ => {
                        let _ = self.repo_list.update(action)?;
                    }
                }
            }

            if self.should_quit {
                tui.exit()?;
                break;
            }
        }
        Ok(())
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        // Ctrl+C always quits, before any overlay or focus-specific routing.
        // Raw mode clears ISIG, so the terminal never raises SIGINT for Ctrl+C
        // — this key binding is the only way it can terminate the app.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.action_tx.send(Action::Quit)?;
            return Ok(());
        }

        // Confirm dialog gets top priority
        if self.confirm_dialog.visible {
            if let Some(action) = self.confirm_dialog.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Path input gets priority
        if self.path_input.visible {
            if let Some(action) = self.path_input.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Theme picker gets priority once visible; blocks the global `t` /
        // help / focus-routed keys until the user commits or cancels.
        if self.theme_picker.visible {
            if let Some(action) = self.theme_picker.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        if self.picker.visible {
            if let Some(action) = self.picker.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Search input gets priority when active
        if self.focus == FocusPanel::Graph && self.git_graph.search_visible() {
            self.git_graph.handle_search_key(key)?;
            return Ok(());
        }

        // Context menu gets priority
        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_key_event(key)? {
                if matches!(action, Action::HideContextMenu) {
                    // fall through to normal handling
                } else {
                    self.action_tx.send(action)?;
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }

        // Help overlay: ? toggles, any other key dismisses
        if key.code == KeyCode::Char('?') {
            self.show_help = !self.show_help;
            return Ok(());
        } else if self.show_help {
            self.show_help = false;
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                // If viewing diff, close it instead of quitting
                if self.focus == FocusPanel::Changes && self.file_list.viewing_diff() {
                    self.file_list.handle_key_event(key)?;
                    return Ok(());
                }
                self.action_tx.send(Action::Quit)?;
            }
            KeyCode::Esc => {
                // Close active detail/diff first, then navigate panels
                if self.focus == FocusPanel::Changes && self.file_list.viewing_diff() {
                    self.file_list.handle_key_event(key)?;
                } else if self.focus == FocusPanel::Graph && self.git_graph.has_detail() {
                    self.git_graph.handle_key_event(key)?;
                } else {
                    match self.focus {
                        FocusPanel::Graph => self.focus = FocusPanel::Changes,
                        FocusPanel::Changes => self.focus = FocusPanel::Repos,
                        FocusPanel::Repos => self.action_tx.send(Action::Quit)?,
                    }
                }
            }
            KeyCode::Tab => {
                // Cycle focus right
                self.focus = match self.focus {
                    FocusPanel::Repos => FocusPanel::Changes,
                    FocusPanel::Changes => FocusPanel::Graph,
                    FocusPanel::Graph => FocusPanel::Repos,
                };
            }
            KeyCode::BackTab => {
                // Cycle focus left
                self.focus = match self.focus {
                    FocusPanel::Repos => FocusPanel::Graph,
                    FocusPanel::Changes => FocusPanel::Repos,
                    FocusPanel::Graph => FocusPanel::Changes,
                };
            }
            KeyCode::Char('r') => {
                self.action_tx.send(Action::RefreshAll)?;
            }
            KeyCode::Char('t') => {
                self.action_tx.send(Action::OpenThemePicker)?;
            }
            KeyCode::Char('R') => {
                self.action_tx.send(Action::RescanRepos)?;
            }
            KeyCode::Char('g') => {
                self.action_tx.send(Action::ShowGitGraph)?;
            }
            KeyCode::Char('G') => {
                self.action_tx.send(Action::GotoSessionSelected)?;
            }
            KeyCode::Char('o') => {
                self.action_tx.send(Action::OpenSelected)?;
            }
            KeyCode::Char('v') => {
                self.action_tx.send(Action::ReviewSelected)?;
            }
            KeyCode::Char('a') => {
                self.action_tx.send(Action::OpenAddRepo)?;
            }
            KeyCode::Char('d') => {
                // On a worktree row, `d` removes that worktree; on a repo row it
                // removes the repo. Both go through a confirmation.
                if self.repo_list.selected_worktree().is_some() {
                    self.action_tx.send(Action::RemoveWorktreeSelected)?;
                } else if let Some(idx) = self.repo_list.selected_index() {
                    let entry = &self.repo_list.repos[idx];
                    let name = entry.name.clone();
                    let repo_id = RepoId(entry.path.clone());
                    self.confirm_dialog
                        .show(format!("Remove {}?", name), Action::RemoveRepo(repo_id));
                }
            }
            KeyCode::Char('s') => {
                self.action_tx.send(Action::CycleSortOrder)?;
            }
            KeyCode::Char('y') => {
                // Copy selected item to clipboard (OSC 52)
                let text = match self.focus {
                    FocusPanel::Repos => self
                        .repo_list
                        .selected_repo()
                        .map(|e| e.path.to_string_lossy().to_string()),
                    FocusPanel::Changes => self.file_list.selected_path(),
                    FocusPanel::Graph => self.git_graph.selected_text(),
                };
                if let Some(text) = text {
                    use std::io::Write;
                    let encoded = base64_encode(text.as_bytes());
                    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                    let _ = std::io::stdout().flush();
                }
            }
            _ => {
                // Route to focused panel
                match self.focus {
                    FocusPanel::Repos => {
                        if let Some(action) = self.repo_list.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                    FocusPanel::Changes => {
                        if let Some(action) = self.file_list.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                    FocusPanel::Graph => {
                        if let Some(action) = self.git_graph.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
        use crossterm::event::{MouseButton, MouseEventKind};

        // Modal overlays swallow mouse input so a click can't leak through to a
        // panel or open a context menu hidden behind them.
        if self.picker.visible
            || self.theme_picker.visible
            || self.path_input.visible
            || self.confirm_dialog.visible
        {
            return Ok(());
        }

        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            } else if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.context_menu.hide();
            }
            return Ok(());
        }

        let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
        const GRAB_ZONE: u16 = 2; // ±2 cells hit zone for border grab

        // Border dragging for panel resize (works in both orientations)
        if self.repo_area.width > 0 {
            // Compute border positions and mouse coordinate along the layout axis
            let (border1, border2, mouse_pos, total, origin) = if self.horizontal_layout {
                (
                    self.repo_area.x + self.repo_area.width,
                    self.changes_area.x + self.changes_area.width,
                    mouse.column,
                    self.repo_area.width + self.changes_area.width + self.graph_area.width,
                    self.repo_area.x,
                )
            } else {
                (
                    self.repo_area.y + self.repo_area.height,
                    self.changes_area.y + self.changes_area.height,
                    mouse.row,
                    self.repo_area.height + self.changes_area.height + self.graph_area.height,
                    self.repo_area.y,
                )
            };

            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let d1 = mouse_pos.abs_diff(border1);
                    let d2 = mouse_pos.abs_diff(border2);
                    if d1 <= GRAB_ZONE && (d1 <= d2 || d2 > GRAB_ZONE) {
                        self.dragging_border = Some(0);
                    } else if d2 <= GRAB_ZONE {
                        self.dragging_border = Some(1);
                    } else {
                        self.dragging_border = None;
                    }
                    // Don't return — let the click propagate to panels
                    // so items near borders remain clickable. The drag
                    // will only engage on MouseEventKind::Drag.
                }
                MouseEventKind::Drag(MouseButton::Left) if self.dragging_border.is_some() => {
                    let rel = mouse_pos.saturating_sub(origin) as f64 / total as f64;
                    let min_f = 3.0 / total as f64;
                    match self.dragging_border {
                        Some(0) => {
                            self.border_frac[0] = rel.clamp(min_f, self.border_frac[1] - min_f);
                        }
                        Some(1) => {
                            self.border_frac[1] =
                                rel.clamp(self.border_frac[0] + min_f, 1.0 - min_f);
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) if self.dragging_border.is_some() => {
                    self.dragging_border = None;
                    return Ok(());
                }
                _ => {}
            }
        }

        // Set focus on left click based on which panel was clicked
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self.repo_area.contains(pos) {
                self.focus = FocusPanel::Repos;
            } else if self.changes_area.contains(pos) {
                self.focus = FocusPanel::Changes;
            } else if self.graph_area.contains(pos) {
                self.focus = FocusPanel::Graph;
            }
        }

        // Route to the panel under the mouse
        if self.repo_area.contains(pos) {
            if let Some(action) = self.repo_list.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            }
        } else if self.changes_area.contains(pos) {
            if let Some(action) = self.file_list.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            }
        } else if self.graph_area.contains(pos)
            && let Some(action) = self.git_graph.handle_mouse_event(mouse)?
        {
            self.action_tx.send(action)?;
        }
        Ok(())
    }

    fn clear_expired_messages(&mut self) -> bool {
        let had_error = self.error_message.is_some();
        let had_success = self.success_message.is_some();

        if let Some((_, when)) = &self.error_message
            && when.elapsed().as_secs() >= 5
        {
            self.error_message = None;
        }
        if let Some((_, when)) = &self.success_message
            && when.elapsed().as_secs() >= 3
        {
            self.success_message = None;
        }

        had_error != self.error_message.is_some() || had_success != self.success_message.is_some()
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) -> Result<()> {
        let area = frame.area();

        // Vertical: main area + status bar
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let main_area = outer[0];
        let status_area = outer[1];

        // Three-panel layout — drag borders to resize in both orientations
        self.horizontal_layout = main_area.width >= 100;
        let (repo_area, changes_area, graph_area) = if self.horizontal_layout {
            let w = main_area.width as f64;
            let c1 = (self.border_frac[0] * w).round() as u16;
            let c2 = ((self.border_frac[1] - self.border_frac[0]) * w).round() as u16;
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(c1),
                    Constraint::Length(c2),
                    Constraint::Min(8),
                ])
                .split(main_area);
            (chunks[0], chunks[1], chunks[2])
        } else {
            let h = main_area.height as f64;
            let r1 = (self.border_frac[0] * h).round() as u16;
            let r2 = ((self.border_frac[1] - self.border_frac[0]) * h).round() as u16;
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(r1),
                    Constraint::Length(r2),
                    Constraint::Min(3),
                ])
                .split(main_area);
            (chunks[0], chunks[1], chunks[2])
        };

        self.repo_area = repo_area;
        self.changes_area = changes_area;
        self.graph_area = graph_area;

        self.repo_list.focused = self.focus == FocusPanel::Repos;
        self.file_list.focused = self.focus == FocusPanel::Changes;
        self.git_graph.focused = self.focus == FocusPanel::Graph;

        self.file_list.horizontal_layout = self.horizontal_layout;
        self.git_graph.horizontal_layout = self.horizontal_layout;

        // Start each render from a blank buffer. Split panels can collapse,
        // logs may have written into the terminal before raw mode, and shorter
        // list/detail content must overwrite old longer content deterministically.
        frame.buffer_mut().reset();

        self.repo_list.draw(frame, repo_area)?;
        self.file_list.draw(frame, changes_area)?;
        self.git_graph.draw(frame, graph_area)?;

        // Paint thick seam borders in horizontal mode to signal "draggable".
        // Vertical mode doesn't need this — the full-width horizontal borders
        // are already easy grab targets, and painting over them destroys titles.
        if self.horizontal_layout {
            use ratatui::style::Style;

            let buf = frame.buffer_mut();
            for (dragging, x_a, x_b) in [
                (
                    self.dragging_border == Some(0),
                    repo_area.x + repo_area.width.saturating_sub(1),
                    changes_area.x,
                ),
                (
                    self.dragging_border == Some(1),
                    changes_area.x + changes_area.width.saturating_sub(1),
                    graph_area.x,
                ),
            ] {
                let color = if dragging {
                    self.theme.overlay.border_drag_active
                } else {
                    self.theme.overlay.border_drag_idle
                };
                let style = Style::default().fg(color);
                for x in [x_a, x_b] {
                    for y in repo_area.y..repo_area.y + repo_area.height {
                        if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                            cell.set_symbol("█");
                            cell.set_style(style);
                        }
                    }
                }
            }
        } else if self.dragging_border.is_some() {
            use ratatui::style::Style;
            let style = Style::default().fg(self.theme.overlay.border_drag_active);
            let buf = frame.buffer_mut();
            for (dragging, y) in [
                (self.dragging_border == Some(0), changes_area.y),
                (self.dragging_border == Some(1), graph_area.y),
            ] {
                if !dragging {
                    continue;
                }
                // Paint just the border characters (skip first col = title area preserved)
                for x in repo_area.x..repo_area.x + repo_area.width {
                    if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                        cell.set_style(style);
                    }
                }
            }
        }

        self.clear_expired_messages();

        self.status_bar.focus = self.focus;
        self.status_bar.sort_order = self.sort_order;
        self.status_bar.error = self.error_message.clone();
        self.status_bar.success = self.success_message.clone();
        self.status_bar.draw(frame, status_area)?;

        // Overlays rendered last
        self.context_menu.draw(frame, area)?;
        self.path_input.draw(frame, area);
        self.confirm_dialog.draw(frame, area);
        self.theme_picker.draw(frame, area);
        self.picker.draw(frame, area);

        // Update notification overlay
        if let Some(ref version) = self.update_version {
            self.draw_update_notification(frame, main_area, version);
        }

        // Help overlay (rendered last so it's on top of everything)
        if self.show_help {
            self.draw_help(frame, main_area);
        }

        Ok(())
    }
}

impl App {
    fn draw_update_notification(&self, frame: &mut ratatui::Frame, area: Rect, version: &str) {
        use ratatui::style::Style;
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let t = &self.theme.overlay;
        let text = format!(" \u{2191} v{version} \u{00b7} gitpane update ");
        let width = text.len() as u16 + 2;
        let height = 3;

        if area.width < width || area.height < height {
            return;
        }

        let x = match self.update_position {
            UpdatePosition::TopRight => area.x + area.width.saturating_sub(width + 1),
            UpdatePosition::TopLeft => area.x + 1,
        };
        let y = area.y;

        let rect = Rect::new(x, y, width, height);

        let line = Line::from(vec![
            Span::styled(" \u{2191} ", Style::default().fg(t.update_toast_arrow)),
            Span::styled(
                format!("v{version}"),
                Style::default().fg(t.update_toast_version),
            ),
            Span::styled(
                " \u{00b7} gitpane update ",
                Style::default().fg(t.update_toast_install),
            ),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.update_toast_border));

        let paragraph = Paragraph::new(line).block(block);

        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(paragraph, rect);
    }

    fn draw_help(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let t = &self.theme.overlay;
        let key = |k: &str| Span::styled(format!("  {k:<10}"), Style::default().fg(t.help_key));
        let desc = |d: &str| Span::raw(d.to_string());
        let section = |title: &str| {
            Line::from(Span::styled(
                format!(" {title}"),
                Style::default().add_modifier(Modifier::BOLD),
            ))
        };

        let mut lines = vec![
            section("Global"),
            Line::from(vec![key("?"), desc("Toggle this help")]),
            Line::from(vec![key("Tab"), desc("Cycle focus forward")]),
            Line::from(vec![key("Shift+Tab"), desc("Cycle focus backward")]),
            Line::from(vec![key("Esc"), desc("Close / go back")]),
            Line::from(vec![key("r"), desc("Refresh all repos")]),
            Line::from(vec![key("o"), desc("Open repo/worktree")]),
            Line::from(vec![key("v"), desc("Review changes (tmux window)")]),
            Line::from(vec![key("G"), desc("Go to live tmux session")]),
            Line::from(vec![key("y"), desc("Copy to clipboard")]),
            Line::from(vec![key("q"), desc("Quit")]),
        ];

        match self.focus {
            FocusPanel::Repos => {
                lines.push(Line::from(""));
                lines.push(section("Repos"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("a"), desc("Add repo")]));
                lines.push(Line::from(vec![
                    key("d"),
                    desc("Remove repo / worktree (confirm)"),
                ]));
                lines.push(Line::from(vec![key("s"), desc("Cycle sort order")]));
                lines.push(Line::from(vec![key("w"), desc("Toggle worktrees")]));
                lines.push(Line::from(vec![
                    key("right-click"),
                    desc("Menu: new worktree, push/pull, …"),
                ]));
                lines.push(Line::from(vec![key("R"), desc("Rescan repos")]));
                lines.push(Line::from(vec![key("g"), desc("Open git graph")]));
            }
            FocusPanel::Changes => {
                lines.push(Line::from(""));
                lines.push(section("Changes"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("Enter"), desc("Open diff view")]));
                lines.push(Line::from(vec![key("Esc / h"), desc("Close diff view")]));
            }
            FocusPanel::Graph => {
                lines.push(Line::from(""));
                lines.push(section("Graph"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("h / l"), desc("Scroll left / right")]));
                lines.push(Line::from(vec![key("Enter"), desc("Open commit files")]));
                lines.push(Line::from(""));
                lines.push(section("Search"));
                lines.push(Line::from(vec![key("/"), desc("Search commits")]));
                lines.push(Line::from(vec![key("n / N"), desc("Next / prev match")]));
                lines.push(Line::from(""));
                lines.push(section("View"));
                lines.push(Line::from(vec![key("f"), desc("First-parent mode")]));
                lines.push(Line::from(vec![key("c"), desc("Collapse / expand branch")]));
                lines.push(Line::from(vec![key("H"), desc("Expand all collapsed")]));
            }
        }

        let height = (lines.len() as u16 + 2).min(area.height);
        let width = 42u16.min(area.width);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let help_area = Rect::new(x, y, width, height);

        let panel_name = match self.focus {
            FocusPanel::Repos => "Repos",
            FocusPanel::Changes => "Changes",
            FocusPanel::Graph => "Graph",
        };
        let block = Block::default()
            .title(format!(" Keybindings \u{2014} {panel_name} "))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.help_border))
            .style(Style::default().bg(t.help_bg));

        frame.render_widget(ratatui::widgets::Clear, help_area);
        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, help_area);
    }
}

/// Simple base64 encoder for OSC 52 clipboard
/// Spawn `argv` detached in `cwd` with null stdio, reaping the child so a
/// fast-exiting launcher (tmux, GUI editor) does not linger as a zombie.
/// Spawn failures are surfaced via `Action::Error`, tagged with `label`
/// ("open" / "review"). Shared by every "launch a command" action.
fn spawn_detached(
    argv: Vec<String>,
    cwd: std::path::PathBuf,
    tx: UnboundedSender<Action>,
    label: &'static str,
) {
    tokio::task::spawn_blocking(move || {
        use std::process::Stdio;
        let child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                let _ = c.wait();
            }
            Err(e) => {
                let _ = tx.send(Action::Error(format!(
                    "{label} failed running '{}': {}",
                    argv[0],
                    crate::git::describe_spawn_error(&e)
                )));
            }
        }
    });
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::StashEntry;

    #[test]
    fn refresh_decision_runs_without_previous_refresh() {
        let now = Instant::now();
        assert_eq!(
            refresh_decision(None, now, Duration::from_millis(1000)),
            RefreshDecision::Now
        );
    }

    #[test]
    fn refresh_decision_delays_inside_cooldown() {
        let last = Instant::now();
        let now = last + Duration::from_millis(250);
        assert_eq!(
            refresh_decision(Some(last), now, Duration::from_millis(1000)),
            RefreshDecision::Later(Duration::from_millis(750))
        );
    }

    #[test]
    fn refresh_decision_runs_after_cooldown() {
        let last = Instant::now();
        let now = last + Duration::from_millis(1000);
        assert_eq!(
            refresh_decision(Some(last), now, Duration::from_millis(1000)),
            RefreshDecision::Now
        );
    }

    #[test]
    fn graph_status_changed_ignores_file_only_changes() {
        let previous = test_status("main", Some("aaa"));
        let next = test_status("main", Some("aaa"));

        // Same HEAD and same rendered refs: a working-tree edit must not reload
        // the graph (the no-churn guarantee that keeps CPU down).
        assert!(!graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::All
        ));
    }

    #[test]
    fn graph_status_changed_detects_head_changes() {
        let previous = test_status("main", Some("aaa"));
        let next = test_status("main", Some("bbb"));

        assert!(graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::All
        ));
    }

    #[test]
    fn graph_status_changed_detects_stash_changes() {
        let previous = test_status("main", Some("aaa"));
        let mut next = test_status("main", Some("aaa"));
        next.stashes.push(StashEntry {
            index: 0,
            message: "WIP on main".to_string(),
            oid: "stash-oid".to_string(),
        });

        assert!(graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::All
        ));
    }

    #[test]
    fn graph_status_changed_detects_commit_on_other_branch() {
        // The worktree case: the root's checked-out HEAD is unchanged, but a
        // commit landed on another local branch (its shared `refs/heads/*` tip
        // moved). The graph renders that branch, so it must reload.
        let previous = test_status("main", Some("aaa"));
        let mut next = test_status("main", Some("aaa"));
        next.refs.local = previous.refs.local ^ 0x9e37_79b9;

        assert!(graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::All
        ));
        // Local-only graphs render local branches too, so they also reload.
        assert!(graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::Local
        ));
    }

    #[test]
    fn graph_status_changed_local_filter_ignores_remote_only_moves() {
        // A background fetch advances a remote-tracking ref. A local-only graph
        // does not draw it, so it must not reload; an all-branches graph does.
        let previous = test_status("main", Some("aaa"));
        let mut next = test_status("main", Some("aaa"));
        next.refs.remote = previous.refs.remote ^ 0x1234_5678;

        assert!(!graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::Local
        ));
        assert!(graph_status_changed(
            Some(&previous),
            &next,
            BranchFilter::All
        ));
    }

    fn test_status(branch: &str, head_oid: Option<&str>) -> RepoStatus {
        RepoStatus {
            branch: branch.to_string(),
            head_oid: head_oid.map(str::to_string),
            files: Vec::new(),
            ahead: 0,
            behind: 0,
            is_dirty: false,
            worktree_info: Vec::new(),
            has_submodules: false,
            submodules: Vec::new(),
            has_dirty_submodules: false,
            has_unpushed_submodules: false,
            fetch_failed: false,
            stashes: Vec::new(),
            refs: crate::git::status::RefsFingerprint::default(),
        }
    }
}
