use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::status::{self, RepoStatus};

#[derive(Clone, Debug)]
pub(crate) struct RepoEntry {
    pub path: PathBuf,
    pub name: String,
    pub status: Option<RepoStatus>,
    pub loading: bool,
}

pub(crate) struct RepoList {
    pub repos: Vec<RepoEntry>,
    pub state: ListState,
    pub render_area: Rect,
    action_tx: Option<UnboundedSender<Action>>,
}

impl RepoList {
    pub fn new(repo_paths: Vec<PathBuf>) -> Self {
        let repos: Vec<RepoEntry> = repo_paths
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                RepoEntry {
                    path,
                    name,
                    status: None,
                    loading: true,
                }
            })
            .collect();

        let mut state = ListState::default();
        if !repos.is_empty() {
            state.select(Some(0));
        }

        Self {
            repos,
            state,
            render_area: Rect::default(),
            action_tx: None,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected()
    }

    pub fn selected_repo(&self) -> Option<&RepoEntry> {
        self.state.selected().and_then(|i| self.repos.get(i))
    }

    fn select_next(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.repos.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn update_status(&mut self, index: usize, repo_status: RepoStatus) {
        if let Some(entry) = self.repos.get_mut(index) {
            entry.status = Some(repo_status);
            entry.loading = false;
        }
    }

    fn spawn_status_queries(&self) {
        let Some(tx) = &self.action_tx else { return };

        for (index, entry) in self.repos.iter().enumerate() {
            let path = entry.path.clone();
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || match status::query_status(&path) {
                Ok(s) => {
                    let _ = tx.send(Action::RepoStatusUpdated { index, status: s });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!(
                        "Failed to query {}: {}",
                        path.display(),
                        e
                    )));
                }
            });
        }
    }
}

impl Component for RepoList {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        self.spawn_status_queries();
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Hit-test against rendered rows (area.y + 1 for border)
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let idx = (mouse.row - content_y) as usize;
                    if idx < self.repos.len() {
                        self.state.select(Some(idx));
                        return Ok(Some(Action::SelectRepo(idx)));
                    }
                }
                Ok(None)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let idx = (mouse.row - content_y) as usize;
                    if idx < self.repos.len() {
                        self.state.select(Some(idx));
                        return Ok(Some(Action::ShowContextMenu {
                            index: idx,
                            row: mouse.row,
                            col: mouse.column,
                        }));
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                self.select_prev();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            _ => Ok(None),
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::SelectNextRepo => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            Action::SelectPrevRepo => {
                self.select_prev();
                let idx = self.state.selected().unwrap_or(0);
                Ok(Some(Action::SelectRepo(idx)))
            }
            Action::RepoStatusUpdated { index, status } => {
                self.update_status(index, status);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;
        let items: Vec<ListItem> = self
            .repos
            .iter()
            .map(|entry| {
                let mut spans = Vec::new();

                match &entry.status {
                    Some(status) => {
                        // Dirty indicator
                        if status.is_dirty {
                            spans.push(Span::styled("* ", Style::default().fg(Color::Yellow)));
                        } else {
                            spans.push(Span::raw("  "));
                        }

                        // Branch name
                        spans.push(Span::styled(
                            format!("{:<12} ", status.branch),
                            Style::default().fg(Color::Cyan),
                        ));

                        // Ahead/behind
                        if status.ahead > 0 || status.behind > 0 {
                            let ab = format!("[+{}/-{}] ", status.ahead, status.behind);
                            spans.push(Span::styled(ab, Style::default().fg(Color::Magenta)));
                        }

                        // Change count
                        if !status.files.is_empty() {
                            spans.push(Span::styled(
                                format!("[{}] ", status.files.len()),
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                    }
                    None => {
                        if entry.loading {
                            spans.push(Span::styled(
                                "  loading... ",
                                Style::default().fg(Color::DarkGray),
                            ));
                        } else {
                            spans.push(Span::raw("  "));
                        }
                    }
                }

                // Repo name
                spans.push(Span::styled(&entry.name, Style::default().fg(Color::White)));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Repositories ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, area, &mut self.state);
        Ok(())
    }
}
