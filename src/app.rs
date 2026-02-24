use color_eyre::Result;
use crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::action::Action;
use crate::components::Component;
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
#[allow(dead_code)]
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
            status_bar: StatusBar::new(),
            right_pane: RightPane::FileList,
            action_tx,
            action_rx,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut tui = Tui::new(self.config.ui.frame_rate)?;
        tui.enter()?;

        // Register action handlers
        self.repo_list
            .register_action_handler(self.action_tx.clone())?;
        self.file_list
            .register_action_handler(self.action_tx.clone())?;
        self.git_graph
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
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.action_tx.send(Action::Quit)?;
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

    fn draw(&mut self, frame: &mut ratatui::Frame) -> Result<()> {
        let area = frame.area();

        // Vertical: main area + status bar
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let main_area = vertical[0];
        let status_area = vertical[1];

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

        self.status_bar.draw(frame, status_area)?;

        Ok(())
    }
}
