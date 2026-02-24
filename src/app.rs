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
pub(crate) enum RightPane {
    FileList,
    GitGraph,
}

pub(crate) struct App {
    config: Config,
    should_quit: bool,
    repo_list: RepoList,
    file_list: FileList,
    git_graph: GitGraph,
    context_menu: ContextMenu,
    status_bar: StatusBar,
    right_pane: RightPane,
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
            right_pane: RightPane::FileList,
            action_tx,
            action_rx,
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

        // If there are repos, auto-select the first one
        if let Some(entry) = self.repo_list.selected_repo()
            && let Some(ref status) = entry.status
        {
            self.file_list.set_files(status.files.clone(), &entry.name);
        }

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
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name);
                        }
                    }
                    Action::RepoStatusUpdated {
                        ref index,
                        ref status,
                    } => {
                        let idx = *index;
                        let status_clone = status.clone();
                        self.repo_list.update_status(idx, status_clone);

                        // Refresh file list if this is the selected repo
                        if self.repo_list.selected_index() == Some(idx)
                            && let Some(entry) = self.repo_list.repos.get(idx)
                        {
                            let name = entry.name.clone();
                            let files = entry
                                .status
                                .as_ref()
                                .map(|s| s.files.clone())
                                .unwrap_or_default();
                            self.file_list.set_files(files, &name);
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
                        self.context_menu.hide();
                        if let Some(entry) = self.repo_list.selected_repo() {
                            let path = entry.path.clone();
                            let name = entry.name.clone();
                            self.git_graph.load_repo(path, &name);
                            self.right_pane = RightPane::GitGraph;
                        }
                    }
                    Action::ShowFileList => {
                        self.right_pane = RightPane::FileList;
                    }
                    Action::GraphLoaded(rows) => {
                        self.git_graph.set_rows(rows);
                    }
                    Action::ShowContextMenu { index, row, col } => {
                        self.context_menu.show(index, col, row);
                    }
                    Action::HideContextMenu => {
                        self.context_menu.hide();
                    }
                    Action::CopyPath(idx) => {
                        if let Some(entry) = self.repo_list.repos.get(idx) {
                            let path_str = entry.path.to_string_lossy().to_string();
                            // Copy to clipboard via OSC 52 escape sequence
                            use std::io::Write;
                            let encoded = base64_encode(path_str.as_bytes());
                            let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Action::Error(ref msg) => {
                        tracing::error!("{}", msg);
                    }
                    _ => {
                        // Forward to components
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
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                self.action_tx.send(Action::Quit)?;
            }
            KeyCode::Esc => {
                if self.right_pane == RightPane::GitGraph {
                    self.action_tx.send(Action::ShowFileList)?;
                } else {
                    self.action_tx.send(Action::Quit)?;
                }
            }
            KeyCode::Char('r') => {
                self.action_tx.send(Action::RefreshAll)?;
            }
            KeyCode::Char('g') => {
                self.action_tx.send(Action::ShowGitGraph)?;
            }
            _ => {
                // Route to active right-pane component or repo list
                if self.right_pane == RightPane::GitGraph {
                    if let Some(action) = self.git_graph.handle_key_event(key)? {
                        self.action_tx.send(action)?;
                    }
                } else if let Some(action) = self.repo_list.handle_key_event(key)? {
                    self.action_tx.send(action)?;
                }
            }
        }
        Ok(())
    }

    fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
        // Context menu gets priority for mouse events
        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            } else {
                // Click outside menu dismisses it
                if matches!(
                    mouse.kind,
                    crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                ) {
                    self.context_menu.hide();
                }
            }
            return Ok(());
        }

        // Route to repo list
        if let Some(action) = self.repo_list.handle_mouse_event(mouse)? {
            self.action_tx.send(action)?;
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) -> Result<()> {
        let area = frame.area();

        // Vertical: main area + status bar
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let main_area = vertical[0];
        let status_area = vertical[1];

        // Responsive: single pane if terminal < 80 cols
        if area.width < 80 {
            self.repo_list.draw(frame, main_area)?;
        } else {
            // Horizontal: repo list (35%) + right pane (65%)
            let horizontal = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(main_area);

            let repo_area = horizontal[0];
            let right_area = horizontal[1];

            self.repo_list.draw(frame, repo_area)?;

            match self.right_pane {
                RightPane::FileList => {
                    self.file_list.draw(frame, right_area)?;
                }
                RightPane::GitGraph => {
                    self.git_graph.draw(frame, right_area)?;
                }
            }
        }

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
