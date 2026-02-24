use color_eyre::Result;
use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use std::time::Instant;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::action::Action;
use crate::components::Component;
use crate::components::context_menu::ContextMenu;
use crate::components::file_list::FileList;
use crate::components::git_graph::GitGraph;
use crate::components::path_input::PathInput;
use crate::components::repo_list::RepoEntry;
use crate::components::repo_list::RepoList;
use crate::components::status_bar::StatusBar;
use crate::config::Config;
use crate::event::Event;
use crate::git::scanner;
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

pub(crate) struct App {
    config: Config,
    should_quit: bool,
    repo_list: RepoList,
    file_list: FileList,
    git_graph: GitGraph,
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
}

impl App {
    pub fn new(config: Config) -> Self {
        let repo_paths = scanner::discover_repos(&config);
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        Self {
            config,
            should_quit: false,
            repo_list: RepoList::new(repo_paths),
            file_list: FileList::new(),
            git_graph: GitGraph::new(),
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
        }
    }

    fn sort_repos(&mut self) {
        match self.sort_order {
            SortOrder::Alphabetical => {
                self.repo_list
                    .repos
                    .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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
            self.repo_list.state.select(Some(0));
        }
    }

    /// Auto-load graph + file list for the selected repo.
    fn sync_selection(&mut self) {
        if let Some(idx) = self.repo_list.selected_index()
            && let Some(entry) = self.repo_list.repos.get(idx)
        {
            let name = entry.name.clone();
            let files = entry
                .status
                .as_ref()
                .map(|s| s.files.clone())
                .unwrap_or_default();
            self.file_list.set_files(files, &name, idx);

            let path = entry.path.clone();
            self.git_graph.load_repo(path, &name);
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut tui = Tui::new()?.mouse(true);
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
        )?;

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
                    Event::RepoChanged(idx) => {
                        self.action_tx.send(Action::RefreshRepo(idx))?;
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
                    Action::SelectRepo(idx) => {
                        self.context_menu.hide();
                        if let Some(entry) = self.repo_list.repos.get(idx) {
                            let name = entry.name.clone();
                            let path = entry.path.clone();
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name, idx);
                            self.git_graph.load_repo(path, &name);
                        }
                    }
                    Action::RepoStatusUpdated {
                        ref index,
                        ref status,
                    } => {
                        let idx = *index;
                        let status_clone = status.clone();
                        self.repo_list.update_status(idx, status_clone);

                        // Refresh file list + graph if this is the selected repo,
                        // but skip if user is actively inspecting (diff or commit detail)
                        if self.repo_list.selected_index() == Some(idx)
                            && !self.file_list.viewing_diff()
                            && !self.git_graph.has_detail()
                            && let Some(entry) = self.repo_list.repos.get(idx)
                        {
                            let name = entry.name.clone();
                            let path = entry.path.clone();
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name, idx);
                            self.git_graph.load_repo(path, &name);
                        }
                    }
                    Action::RefreshAll => {
                        // User-initiated refresh: fetch from remote + show spinner
                        for (idx, entry) in self.repo_list.repos.iter_mut().enumerate() {
                            entry.git_op = true;
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                match crate::git::status::query_status_with_fetch(&path) {
                                    Ok(s) => {
                                        let _ = tx.send(Action::RepoStatusUpdated {
                                            index: idx,
                                            status: s,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx
                                            .send(Action::Error(format!("Failed to query: {}", e)));
                                    }
                                }
                            });
                        }
                    }
                    Action::RefreshRepo(idx) => {
                        // Watcher-triggered: fast local-only, no spinner
                        if let Some(entry) = self.repo_list.repos.get_mut(idx) {
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                match crate::git::status::query_status(&path) {
                                    Ok(s) => {
                                        let _ = tx.send(Action::RepoStatusUpdated {
                                            index: idx,
                                            status: s,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx
                                            .send(Action::Error(format!("Failed to query: {}", e)));
                                    }
                                }
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
                    Action::GraphLoaded(rows) => {
                        self.git_graph.set_rows(rows);
                    }
                    Action::GraphError(ref msg) => {
                        self.git_graph.set_error(msg.clone());
                    }
                    Action::ShowContextMenu { index, row, col } => {
                        let (ahead, behind) = self
                            .repo_list
                            .repos
                            .get(index)
                            .and_then(|e| e.status.as_ref())
                            .map(|s| (s.ahead, s.behind))
                            .unwrap_or((0, 0));
                        self.context_menu.show(index, col, row, ahead, behind);
                    }
                    Action::HideContextMenu => {
                        self.context_menu.hide();
                    }
                    Action::CopyPath(idx) => {
                        if let Some(entry) = self.repo_list.repos.get(idx) {
                            let path_str = entry.path.to_string_lossy().to_string();
                            use std::io::Write;
                            let encoded = base64_encode(path_str.as_bytes());
                            let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Action::GitPush(idx) | Action::GitPull(idx) | Action::GitPullRebase(idx) => {
                        if let Some(entry) = self.repo_list.repos.get_mut(idx) {
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
                                _ => unreachable!(),
                            };
                            // Add origin <branch> so pull/push works even without upstream config
                            if !branch.is_empty() && branch != "(no branch)" {
                                git_args.push("origin".into());
                                git_args.push(branch);
                            }
                            entry.git_op = true;
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            tokio::task::spawn_blocking(move || {
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&path)
                                    .args(&git_args)
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        let _ = tx.send(Action::GitOpComplete {
                                            index: idx,
                                            message: format!(
                                                "git {} succeeded",
                                                git_args.join(" ")
                                            ),
                                        });
                                    }
                                    Ok(o) => {
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
                                        let _ = tx.send(Action::RefreshRepo(idx));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            git_args.join(" "),
                                            e
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(idx));
                                    }
                                }
                            });
                        }
                    }
                    Action::GitOpComplete { index, ref message } => {
                        self.success_message = Some((message.clone(), Instant::now()));
                        self.action_tx.send(Action::RefreshRepo(index))?;
                    }
                    Action::ShowDiff(repo_idx, ref file_path) => {
                        if let Some(entry) = self.repo_list.repos.get(repo_idx) {
                            let path = entry.path.clone();
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
                                        let _ = tx.send(Action::DiffLoaded(text));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Action::DiffLoaded(format!(
                                            "Failed to get diff: {}",
                                            e
                                        )));
                                    }
                                }
                            });
                        }
                    }
                    Action::DiffLoaded(ref content) => {
                        self.file_list.set_diff(content.clone());
                    }
                    Action::ShowCommitFiles {
                        ref repo_path,
                        ref oid,
                    } => {
                        let path = repo_path.clone();
                        let oid = oid.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            match crate::git::commit_files::list_commit_files(&path, &oid) {
                                Ok(files) => {
                                    let _ = tx.send(Action::CommitFilesLoaded { oid, files });
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
                    Action::CommitFilesLoaded { ref oid, ref files } => {
                        self.git_graph.set_commit_files(oid.clone(), files.clone());
                    }
                    Action::ShowCommitDiff {
                        ref repo_path,
                        ref oid,
                        ref file_path,
                    } => {
                        let path = repo_path.clone();
                        let oid = oid.clone();
                        let fp = file_path.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            match crate::git::commit_files::commit_file_diff(&path, &oid, &fp) {
                                Ok(diff) => {
                                    let _ = tx.send(Action::CommitDiffLoaded(diff));
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
                    Action::CommitDiffLoaded(ref content) => {
                        self.git_graph.set_commit_diff(content.clone());
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
                            self.repo_list.repos.push(RepoEntry {
                                path,
                                name,
                                status: None,
                                git_op: false,
                            });
                            let idx = self.repo_list.repos.len() - 1;
                            self.action_tx.send(Action::RefreshRepo(idx))?;
                            self.action_tx.send(Action::SelectRepo(idx))?;
                        }
                    }
                    Action::RemoveRepo(idx) => {
                        if idx < self.repo_list.repos.len() {
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
                                self.file_list.set_files(Vec::new(), "", 0);
                            } else {
                                let new_idx = idx.min(self.repo_list.repos.len() - 1);
                                self.repo_list.state.select(Some(new_idx));
                                self.action_tx.send(Action::SelectRepo(new_idx))?;
                            }
                        }
                    }
                    Action::CycleSortOrder => {
                        self.sort_order = self.sort_order.next();
                        self.sort_repos();
                        self.sync_selection();
                    }
                    Action::RescanRepos => {
                        // Clear user-added exclusions, save, and re-discover repos
                        self.config.excluded_repos.clear();
                        if let Err(e) = self.config.save() {
                            tracing::error!("Failed to save config: {}", e);
                        }
                        let repo_paths = scanner::discover_repos(&self.config);
                        self.repo_list = RepoList::new(repo_paths);
                        self.repo_list
                            .register_action_handler(self.action_tx.clone())?;
                        self.repo_list.init()?;
                        self.sort_repos();
                        self.sync_selection();
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
        // Path input gets top priority
        if self.path_input.visible {
            if let Some(action) = self.path_input.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
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
                    self.action_tx.send(Action::RemoveRepo(idx))?;
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

        // Three-panel layout
        let (repo_area, changes_area, graph_area) = if area.width < 100 {
            // Narrow: vertical stack
            let v = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(30),
                    Constraint::Percentage(40),
                ])
                .split(main_area);
            (v[0], v[1], v[2])
        } else {
            // Wide: horizontal
            let h = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(25),
                    Constraint::Percentage(25),
                    Constraint::Percentage(50),
                ])
                .split(main_area);
            (h[0], h[1], h[2])
        };

        self.repo_area = repo_area;
        self.changes_area = changes_area;
        self.graph_area = graph_area;

        self.repo_list.focused = self.focus == FocusPanel::Repos;
        self.file_list.focused = self.focus == FocusPanel::Changes;
        self.git_graph.focused = self.focus == FocusPanel::Graph;

        self.repo_list.draw(frame, repo_area)?;
        self.file_list.draw(frame, changes_area)?;
        self.git_graph.draw(frame, graph_area)?;

        self.status_bar.focus = self.focus;
        self.status_bar.sort_order = self.sort_order;
        self.status_bar.error = self.error_message.clone();
        self.status_bar.success = self.success_message.clone();
        self.status_bar.draw(frame, status_area)?;

        // Overlays rendered last
        self.context_menu.draw(frame, area)?;
        self.path_input.draw(frame, area);

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
