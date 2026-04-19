use color_eyre::Result;
use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::action::Action;
use crate::components::Component;
use crate::components::confirm_dialog::ConfirmDialog;
use crate::components::context_menu::ContextMenu;
use crate::components::file_list::FileList;
use crate::components::git_graph::GitGraph;
use crate::components::path_input::PathInput;
use crate::components::repo_list::RepoEntry;
use crate::components::repo_list::RepoList;
use crate::components::status_bar::StatusBar;
use crate::config::Config;
use crate::config::UpdatePosition;
use crate::event::Event;
use crate::git::graph::GraphOptions;
use crate::git::scanner;
use crate::repo_id::RepoId;
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
    /// [0] = repos/changes split, [1] = changes/graph split.
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
    /// When a worktree row is selected, stores context for diff/status routing
    /// and live-polling the worktree's changes.
    active_worktree: Option<ActiveWorktree>,
}

#[derive(Clone)]
struct ActiveWorktree {
    path: std::path::PathBuf,
    repo_id: RepoId,
    display_name: String,
}

impl App {
    pub fn new(config: Config) -> Self {
        let repo_paths = scanner::discover_repos(&config);
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        let mut git_graph = GitGraph::new();
        git_graph.graph_options = GraphOptions {
            branch_filter: config.graph.branches,
            label_max_len: config.graph.label_max_len,
            first_parent: false,
            show_stats: config.graph.show_stats,
        };

        let update_position = config.ui.update_position;
        let ignore_dirty_subs = config.submodules.ignore_dirty;
        let poll_semaphore = Arc::new(tokio::sync::Semaphore::new(
            config.watch.max_concurrent_polls,
        ));

        Self {
            config,
            should_quit: false,
            repo_list: RepoList::new(repo_paths, ignore_dirty_subs),
            file_list: FileList::new(),
            git_graph,
            confirm_dialog: ConfirmDialog::new(),
            context_menu: ContextMenu::new(),
            path_input: PathInput::new(),
            status_bar: StatusBar::new(),
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
            active_worktree: None,
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

        // Init components
        self.repo_list.init()?;

        // Trigger immediate status poll so repos don't show "..." until the
        // first PollLocal timer fires. Goes through the semaphore-controlled path.
        self.action_tx.send(Action::PollLocal)?;

        // Start filesystem watcher
        let repo_paths: Vec<_> = self
            .repo_list
            .repos
            .iter()
            .map(|r| r.path.clone())
            .collect();
        let _watcher = RepoWatcher::new(
            &repo_paths,
            self.config.watch.debounce_ms,
            tui.event_tx.clone(),
            &self.config.watch.watch_exclude_dirs,
        )?;

        // Check for updates in the background
        if self.config.ui.check_for_updates {
            let tx = self.action_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(version) = crate::update_checker::check_latest() {
                    let _ = tx.send(Action::UpdateAvailable(version));
                }
            });
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
                        self.action_tx.send(Action::Tick)?;
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
                        let ignore_dirty_subs = self.config.submodules.ignore_dirty;
                        tokio::task::spawn_blocking(
                            move || match crate::git::status::query_status(
                                &wt_path,
                                ignore_dirty_subs,
                            ) {
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
                        // Clear git_op so the repo isn't permanently skipped
                        // by future polls after a failed status query.
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            self.repo_list.repos[idx].git_op = false;
                        }
                        if self.dirty_repos.remove(id) {
                            self.action_tx.send(Action::RefreshRepo(id.clone()))?;
                        }
                    }
                    Action::RepoStatusUpdated { ref id, ref status } => {
                        self.pending_status.remove(id);
                        let is_dirty = self.dirty_repos.remove(id);
                        if let Some(idx) = self.repo_list.resolve_index(id) {
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

                                if self.git_graph.has_detail() {
                                    self.git_graph.set_needs_reload();
                                } else {
                                    let path = entry.path.clone();
                                    self.git_graph.load_repo(path, &name);
                                }
                            }
                        }
                        if is_dirty {
                            self.action_tx.send(Action::RefreshRepo(id.clone()))?;
                        }
                    }
                    Action::RefreshAll => {
                        // User-initiated refresh: fetch from remote + show spinner
                        let ignore_dirty_subs = self.config.submodules.ignore_dirty;
                        for entry in self.repo_list.repos.iter_mut() {
                            entry.git_op = true;
                            let repo_id = RepoId(entry.path.clone());
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status_with_fetch(
                                        &path,
                                        ignore_dirty_subs,
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
                        // Fast local status poll (no network, no spinner)
                        let ignore_dirty_subs = self.config.submodules.ignore_dirty;
                        for entry in self.repo_list.repos.iter() {
                            let repo_id = RepoId(entry.path.clone());
                            if entry.git_op || self.pending_status.contains(&repo_id) {
                                continue;
                            }
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status(&path, ignore_dirty_subs)
                                    {
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
                        if let Some(aw) = self.active_worktree.clone() {
                            // Refresh the graph from the worktree so new commits appear live
                            if self.git_graph.has_detail() {
                                self.git_graph.set_needs_reload();
                            } else {
                                self.git_graph.load_repo(aw.path.clone(), &aw.display_name);
                            }

                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Ok(s) =
                                    crate::git::status::query_status(&aw.path, ignore_dirty_subs)
                                {
                                    let _ = tx.send(Action::WorktreeFilesLoaded {
                                        repo_id: aw.repo_id,
                                        worktree_path: aw.path,
                                        name: aw.display_name,
                                        files: s.files,
                                    });
                                }
                            });
                        }
                    }
                    Action::PollFetch => {
                        // Remote fetch poll (updates ahead/behind, no spinner)
                        let ignore_dirty_subs = self.config.submodules.ignore_dirty;
                        for entry in self.repo_list.repos.iter() {
                            let repo_id = RepoId(entry.path.clone());
                            if entry.git_op || self.pending_status.contains(&repo_id) {
                                continue;
                            }
                            self.pending_status.insert(repo_id.clone());
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status_with_fetch(
                                        &path,
                                        ignore_dirty_subs,
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
                        // Watcher-triggered: fast local-only, no spinner
                        if self.pending_status.contains(id) {
                            self.dirty_repos.insert(id.clone());
                            tracing::debug!(
                                "skipping repo {}: already in-flight (marked dirty)",
                                id
                            );
                            continue;
                        }
                        let ignore_dirty_subs = self.config.submodules.ignore_dirty;
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let repo_id = id.clone();
                            self.pending_status.insert(repo_id.clone());
                            let path = self.repo_list.repos[idx].path.clone();
                            let tx = self.action_tx.clone();
                            let sem = self.poll_semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await;
                                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                                tokio::task::spawn_blocking(move || {
                                    match crate::git::status::query_status(&path, ignore_dirty_subs)
                                    {
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
                    Action::ShowGitGraph => {
                        // Force-reload graph for selected repo
                        self.context_menu.hide();
                        if let Some(entry) = self.repo_list.selected_repo() {
                            let path = entry.path.clone();
                            let name = entry.name.clone();
                            self.git_graph.load_repo(path, &name);
                        }
                        self.focus = FocusPanel::Graph;
                    }
                    Action::ShowFileList => {
                        self.focus = FocusPanel::Changes;
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
                    Action::GraphError(ref msg) => {
                        self.git_graph.set_error(msg.clone());
                    }
                    Action::ShowContextMenu { ref id, row, col } => {
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let (ahead, behind, has_submodules) = self.repo_list.repos[idx]
                                .status
                                .as_ref()
                                .map(|s| (s.ahead, s.behind, s.has_submodules))
                                .unwrap_or((0, 0, false));
                            self.context_menu.show(
                                id.clone(),
                                col,
                                row,
                                ahead,
                                behind,
                                has_submodules,
                            );
                        }
                    }
                    Action::HideContextMenu => {
                        self.context_menu.hide();
                    }
                    Action::CopyPath(ref id) => {
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &self.repo_list.repos[idx];
                            let path_str = entry.path.to_string_lossy().to_string();
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
                        if let Some(idx) = self.repo_list.resolve_index(id) {
                            let entry = &mut self.repo_list.repos[idx];
                            let branch = entry
                                .status
                                .as_ref()
                                .map(|s| s.branch.clone())
                                .unwrap_or_default();
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
                                            e
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(repo_id));
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
                                            e
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
                                        == crate::git::status::SubmoduleState::Dirty
                                        || !pointer_changed;

                                    if use_diff {
                                        let header = format!(
                                            "Submodule {} ({})\n{}\n",
                                            sub_path.display(),
                                            match sub_state {
                                                crate::git::status::SubmoduleState::Dirty => "uncommitted changes",
                                                crate::git::status::SubmoduleState::Uninitialized => "not initialized",
                                                crate::git::status::SubmoduleState::Modified => "modified",
                                            },
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
                                                format!("Failed to get submodule diff: {}", e)
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
                                            Err(e) => format!("Failed to get submodule log: {}", e),
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
                                                content: format!("Failed to get diff: {}", e),
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
                        // Clear user-added exclusions, save, and re-discover repos
                        self.config.excluded_repos.clear();
                        if let Err(e) = self.config.save() {
                            tracing::error!("Failed to save config: {}", e);
                        }
                        let repo_paths = scanner::discover_repos(&self.config);
                        self.repo_list =
                            RepoList::new(repo_paths, self.config.submodules.ignore_dirty);
                        self.repo_list
                            .register_action_handler(self.action_tx.clone())?;
                        self.repo_list.init()?;
                        self.action_tx.send(Action::PollLocal)?;
                        self.sort_repos();
                        self.sync_selection();
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
            KeyCode::Char('R') => {
                self.action_tx.send(Action::RescanRepos)?;
            }
            KeyCode::Char('g') => {
                self.action_tx.send(Action::ShowGitGraph)?;
            }
            KeyCode::Char('a') => {
                self.action_tx.send(Action::OpenAddRepo)?;
            }
            KeyCode::Char('d') => {
                if let Some(idx) = self.repo_list.selected_index() {
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

        self.repo_list.draw(frame, repo_area)?;
        self.file_list.draw(frame, changes_area)?;
        self.git_graph.draw(frame, graph_area)?;

        // Paint thick seam borders in horizontal mode to signal "draggable".
        // Vertical mode doesn't need this — the full-width horizontal borders
        // are already easy grab targets, and painting over them destroys titles.
        if self.horizontal_layout {
            use ratatui::style::{Color, Style};

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
                    Color::Yellow
                } else {
                    Color::DarkGray
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
            // In vertical mode, only highlight the seam during active drag
            use ratatui::style::{Color, Style};

            let style = Style::default().fg(Color::Yellow);
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

        // Clear timed-out messages so they don't keep re-appearing
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

        self.status_bar.focus = self.focus;
        self.status_bar.sort_order = self.sort_order;
        self.status_bar.error = self.error_message.clone();
        self.status_bar.success = self.success_message.clone();
        self.status_bar.draw(frame, status_area)?;

        // Overlays rendered last
        self.context_menu.draw(frame, area)?;
        self.path_input.draw(frame, area);
        self.confirm_dialog.draw(frame, area);

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
        use ratatui::style::{Color, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let text = format!(" \u{2191} v{version} \u{00b7} cargo install gitpane ");
        let width = text.len() as u16 + 2; // +2 for border
        let height = 3; // top border + content + bottom border

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
            Span::styled(" \u{2191} ", Style::default().fg(Color::Green)),
            Span::styled(format!("v{version}"), Style::default().fg(Color::Yellow)),
            Span::styled(
                " \u{00b7} cargo install gitpane ",
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let paragraph = Paragraph::new(line).block(block);

        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(paragraph, rect);
    }

    fn draw_help(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let key = |k: &str| Span::styled(format!("  {k:<10}"), Style::default().fg(Color::Yellow));
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
            Line::from(vec![key("y"), desc("Copy to clipboard")]),
            Line::from(vec![key("q"), desc("Quit")]),
        ];

        match self.focus {
            FocusPanel::Repos => {
                lines.push(Line::from(""));
                lines.push(section("Repos"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("a"), desc("Add repo")]));
                lines.push(Line::from(vec![key("d"), desc("Remove repo (confirm)")]));
                lines.push(Line::from(vec![key("s"), desc("Cycle sort order")]));
                lines.push(Line::from(vec![key("w"), desc("Toggle worktrees")]));
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
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Black));

        frame.render_widget(ratatui::widgets::Clear, help_area);
        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, help_area);
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
