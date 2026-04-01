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
use crate::git::status::RepoStatus;
use crate::repo_id::RepoId;

#[derive(Clone, Debug)]
pub(crate) struct RepoEntry {
    pub path: PathBuf,
    pub name: String,
    pub status: Option<RepoStatus>,
    /// True only during push/pull/rebase — shows animated spinner
    pub git_op: bool,
}

pub(crate) struct RepoList {
    pub repos: Vec<RepoEntry>,
    pub state: ListState,
    pub render_area: Rect,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
}

impl RepoList {
    pub fn new(repo_paths: Vec<PathBuf>, _ignore_dirty_subs: bool) -> Self {
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
                    git_op: false,
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
            focused: true,
            action_tx: None,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Resolve a stable `RepoId` to its current positional index.
    pub fn resolve_index(&self, id: &RepoId) -> Option<usize> {
        self.repos.iter().position(|e| e.path == id.0)
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
            entry.git_op = false;
        }
    }
}

impl Component for RepoList {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        // Initial status queries are triggered by App via PollLocal
        // to ensure they go through the shared semaphore and pending_status.
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                let idx = self.state.selected().unwrap_or(0);
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.repos.len() {
                        self.state.select(Some(idx));
                        let id = RepoId(self.repos[idx].path.clone());
                        return Ok(Some(Action::SelectRepo(id)));
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
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.repos.len() {
                        self.state.select(Some(idx));
                        let id = RepoId(self.repos[idx].path.clone());
                        return Ok(Some(Action::ShowContextMenu {
                            id,
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
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            _ => Ok(None),
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::SelectNextRepo => {
                self.select_next();
                let idx = self.state.selected().unwrap_or(0);
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            Action::SelectPrevRepo => {
                self.select_prev();
                let idx = self.state.selected().unwrap_or(0);
                let id = RepoId(self.repos[idx].path.clone());
                Ok(Some(Action::SelectRepo(id)))
            }
            Action::RepoStatusUpdated { ref id, ref status } => {
                if let Some(idx) = self.resolve_index(id) {
                    self.update_status(idx, status.clone());
                }
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

                // Dirty / git-op indicator
                if entry.git_op {
                    spans.push(Span::styled("~ ", Style::default().fg(Color::Cyan)));
                } else if entry.status.as_ref().map(|s| s.is_dirty).unwrap_or(false) {
                    spans.push(Span::styled("* ", Style::default().fg(Color::Yellow)));
                } else {
                    spans.push(Span::raw("  "));
                }

                if let Some(status) = &entry.status {
                    // Branch name
                    spans.push(Span::styled(
                        format!("{:<12} ", status.branch),
                        Style::default().fg(Color::Cyan),
                    ));

                    // Ahead/behind (VSCode-style ↑↓ arrows)
                    if status.ahead > 0 {
                        spans.push(Span::styled(
                            format!("↑{} ", status.ahead),
                            Style::default().fg(Color::Green),
                        ));
                    }
                    if status.behind > 0 {
                        spans.push(Span::styled(
                            format!("↓{} ", status.behind),
                            Style::default().fg(Color::Red),
                        ));
                    }

                    // Worktree count (linked worktrees, e.g. from agentic AI)
                    if status.worktrees > 0 {
                        spans.push(Span::styled(
                            format!("⎇{} ", status.worktrees),
                            Style::default().fg(Color::Magenta),
                        ));
                    }

                    // Dirty submodule indicator
                    if status.has_dirty_submodules {
                        spans.push(Span::styled("◈ ", Style::default().fg(Color::LightMagenta)));
                    }

                    // Fetch failure indicator
                    if status.fetch_failed {
                        spans.push(Span::styled("⚠ ", Style::default().fg(Color::DarkGray)));
                    }

                    // Change count
                    if !status.files.is_empty() {
                        spans.push(Span::styled(
                            format!("[{}] ", status.files.len()),
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                }

                // Repo name
                spans.push(Span::styled(&entry.name, Style::default().fg(Color::White)));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let border_color = if self.focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Repositories ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
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
