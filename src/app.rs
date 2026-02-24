use color_eyre::Result;
use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::action::Action;
use crate::components::Component;
use crate::components::context_menu::ContextMenu;
use crate::components::file_list::FileList;
use crate::components::git_graph::GitGraph;
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

pub(crate) struct App {
    config: Config,
    should_quit: bool,
    repo_list: RepoList,
    file_list: FileList,
    git_graph: GitGraph,
    context_menu: ContextMenu,
    status_bar: StatusBar,
    focus: FocusPanel,
    action_tx: UnboundedSender<Action>,
    action_rx: UnboundedReceiver<Action>,
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
            status_bar: StatusBar::new(),
            focus: FocusPanel::Repos,
            action_tx,
            action_rx,
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
        let mut tui = Tui::new(self.config.ui.frame_rate)?.mouse(true);
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

                        // Refresh file list + graph if this is the selected repo
                        if self.repo_list.selected_index() == Some(idx)
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
                        for entry in &mut self.repo_list.repos {
                            entry.loading = true;
                        }
                        self.repo_list.init()?;
                    }
                    Action::RefreshRepo(idx) => {
                        if let Some(entry) = self.repo_list.repos.get_mut(idx) {
                            entry.loading = true;
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
                        let git_args: Vec<&str> = match action {
                            Action::GitPush(_) => vec!["push"],
                            Action::GitPull(_) => vec!["pull"],
                            Action::GitPullRebase(_) => vec!["pull", "--rebase"],
                            _ => unreachable!(),
                        };
                        if let Some(entry) = self.repo_list.repos.get_mut(idx) {
                            entry.loading = true;
                            let path = entry.path.clone();
                            let tx = self.action_tx.clone();
                            let args: Vec<String> =
                                git_args.iter().map(|s| s.to_string()).collect();
                            tokio::task::spawn_blocking(move || {
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&path)
                                    .args(&args)
                                    .output();
                                match output {
                                    Ok(o) if o.status.success() => {
                                        let _ = tx.send(Action::GitOpComplete {
                                            index: idx,
                                            message: format!("git {} succeeded", args.join(" ")),
                                        });
                                    }
                                    Ok(o) => {
                                        let stderr = String::from_utf8_lossy(&o.stderr);
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            args.join(" "),
                                            stderr.trim()
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(idx));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Action::Error(format!(
                                            "git {} failed: {}",
                                            args.join(" "),
                                            e
                                        )));
                                        let _ = tx.send(Action::RefreshRepo(idx));
                                    }
                                }
                            });
                        }
                    }
                    Action::GitOpComplete { index, .. } => {
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
                    Action::Error(ref msg) => {
                        tracing::error!("{}", msg);
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

        // Diff view gets priority
        if self.file_list.viewing_diff() {
            if let Some(action) = self.file_list.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                self.action_tx.send(Action::Quit)?;
            }
            KeyCode::Esc => {
                // Move focus left, or quit from repos
                match self.focus {
                    FocusPanel::Graph => self.focus = FocusPanel::Changes,
                    FocusPanel::Changes => self.focus = FocusPanel::Repos,
                    FocusPanel::Repos => self.action_tx.send(Action::Quit)?,
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
            KeyCode::Char('g') => {
                self.action_tx.send(Action::ShowGitGraph)?;
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
        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            } else if matches!(
                mouse.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            ) {
                self.context_menu.hide();
            }
            return Ok(());
        }

        if let Some(action) = self.repo_list.handle_mouse_event(mouse)? {
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

        self.repo_list.focused = self.focus == FocusPanel::Repos;
        self.file_list.focused = self.focus == FocusPanel::Changes;
        self.git_graph.focused = self.focus == FocusPanel::Graph;

        self.repo_list.draw(frame, repo_area)?;
        self.file_list.draw(frame, changes_area)?;
        self.git_graph.draw(frame, graph_area)?;

        self.status_bar.focus = self.focus;
        self.status_bar.draw(frame, status_area)?;

        // Context menu rendered last (overlay)
        self.context_menu.draw(frame, area)?;

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
