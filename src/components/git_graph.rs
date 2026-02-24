use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::graph::{GraphBuilder, GraphRow};
use crate::git::graph_render;

struct CommitDetail {
    oid: String,
    files: Vec<(String, String)>,
    file_state: ListState,
    diff_content: Option<String>,
    diff_scroll: u16,
}

pub(crate) struct GitGraph {
    rows: Vec<GraphRow>,
    state: ListState,
    repo_name: String,
    repo_path: Option<PathBuf>,
    loading: bool,
    error: Option<String>,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    render_area: Rect,
    graph_list_area: Rect,
    files_area: Rect,
    diff_area: Rect,
    commit_detail: Option<CommitDetail>,
}

impl GitGraph {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
            repo_path: None,
            loading: false,
            error: None,
            focused: false,
            action_tx: None,
            render_area: Rect::default(),
            graph_list_area: Rect::default(),
            files_area: Rect::default(),
            diff_area: Rect::default(),
            commit_detail: None,
        }
    }

    pub fn load_repo(&mut self, path: PathBuf, repo_name: &str) {
        self.repo_name = repo_name.to_string();
        self.repo_path = Some(path.clone());
        self.loading = true;
        self.error = None;
        self.rows.clear();
        self.state.select(None);
        self.commit_detail = None;

        let Some(tx) = &self.action_tx else { return };
        let tx = tx.clone();

        tokio::task::spawn_blocking(move || {
            let builder = GraphBuilder::new();
            match builder.build(&path) {
                Ok(rows) => {
                    let _ = tx.send(Action::GraphLoaded(rows));
                }
                Err(e) => {
                    let _ = tx.send(Action::GraphError(format!("Failed to load graph: {}", e)));
                }
            }
        });
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        self.loading = false;
    }

    pub fn set_rows(&mut self, rows: Vec<GraphRow>) {
        self.rows = rows;
        self.loading = false;
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn set_commit_files(&mut self, oid: String, files: Vec<(String, String)>) {
        let mut file_state = ListState::default();
        if !files.is_empty() {
            file_state.select(Some(0));
        }
        self.commit_detail = Some(CommitDetail {
            oid,
            files,
            file_state,
            diff_content: None,
            diff_scroll: 0,
        });
    }

    pub fn set_commit_diff(&mut self, content: String) {
        if let Some(ref mut detail) = self.commit_detail {
            detail.diff_content = Some(content);
            detail.diff_scroll = 0;
        }
    }

    pub fn has_detail(&self) -> bool {
        self.commit_detail.is_some()
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.rows.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn try_show_commit_files(&self) -> Option<Action> {
        let idx = self.state.selected()?;
        let row = self.rows.get(idx)?;
        let repo_path = self.repo_path.clone()?;
        Some(Action::ShowCommitFiles {
            repo_path,
            oid: row.oid.to_string(),
        })
    }

    fn try_show_commit_diff(&self) -> Option<Action> {
        let detail = self.commit_detail.as_ref()?;
        let file_idx = detail.file_state.selected()?;
        let (_, file_path) = detail.files.get(file_idx)?;
        let repo_path = self.repo_path.clone()?;
        Some(Action::ShowCommitDiff {
            repo_path,
            oid: detail.oid.clone(),
            file_path: file_path.clone(),
        })
    }

    fn draw_graph_list(&mut self, frame: &mut Frame, area: Rect) {
        let title = format!(" Git Graph — {} ", self.repo_name);
        let border_color = if self.focused && self.commit_detail.is_none() {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.loading {
            let paragraph = Paragraph::new("Loading graph...")
                .style(Style::default().fg(Color::Yellow))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if let Some(ref err) = self.error {
            let paragraph = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if self.rows.is_empty() {
            let paragraph = Paragraph::new("No commits")
                .style(Style::default().fg(Color::Gray))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let mut spans = graph_render::render_graph_prefix(row);

                spans.push(Span::styled(
                    format!("{} ", row.short_id),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));

                spans.push(Span::styled(
                    &row.message,
                    Style::default().fg(Color::White),
                ));

                spans.push(Span::styled(
                    format!("  — {}", row.author),
                    Style::default().fg(Color::DarkGray),
                ));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_commit_files(detail: &mut CommitDetail, frame: &mut Frame, area: Rect) {
        let title = format!(" Files — {} ", &detail.oid[..7.min(detail.oid.len())]);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        if detail.files.is_empty() {
            let paragraph = Paragraph::new("No files changed")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = detail
            .files
            .iter()
            .map(|(status, path)| {
                let color = match status.as_str() {
                    "M" => Color::Yellow,
                    "A" => Color::Green,
                    "D" => Color::Red,
                    "R" => Color::Blue,
                    _ => Color::DarkGray,
                };
                let spans = vec![
                    Span::styled(
                        format!(" {} ", status),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(path, Style::default().fg(Color::White)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut detail.file_state);
    }

    fn draw_commit_diff(detail: &CommitDetail, frame: &mut Frame, area: Rect) {
        let Some(ref content) = detail.diff_content else {
            return;
        };

        let block = Block::default()
            .title(" Commit Diff (Esc to close) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let lines: Vec<Line> = content
            .lines()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("@@") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                };
                Line::from(Span::styled(line, style))
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((detail.diff_scroll, 0));

        frame.render_widget(paragraph, area);
    }
}

impl Component for GitGraph {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        // When detail is open, Esc/keys are layered
        if let Some(ref mut detail) = self.commit_detail {
            if detail.diff_content.is_some() {
                // Viewing commit diff
                match key.code {
                    KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => {
                        detail.diff_content = None;
                        detail.diff_scroll = 0;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        detail.diff_scroll = detail.diff_scroll.saturating_add(1);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        detail.diff_scroll = detail.diff_scroll.saturating_sub(1);
                    }
                    _ => {}
                }
                return Ok(None);
            }

            // Viewing commit file list
            match key.code {
                KeyCode::Esc => {
                    self.commit_detail = None;
                    return Ok(None);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| (i + 1).min(detail.files.len() - 1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                    }
                    return Ok(None);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| i.saturating_sub(1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                    }
                    return Ok(None);
                }
                KeyCode::Enter => {
                    return Ok(self.try_show_commit_diff());
                }
                _ => return Ok(None),
            }
        }

        // No detail open — normal graph navigation
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.try_show_commit_files()),
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);

                // Click in graph list area
                if self.graph_list_area.contains(pos) {
                    let content_y = self.graph_list_area.y + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + self.state.offset();
                        if idx < self.rows.len() {
                            // Click on already-selected row opens commit files
                            if self.state.selected() == Some(idx) && self.commit_detail.is_none() {
                                return Ok(self.try_show_commit_files());
                            }
                            self.state.select(Some(idx));
                            self.commit_detail = None;
                        }
                    }
                    return Ok(None);
                }

                // Click in commit files area
                let mut open_file_diff = false;
                if let Some(ref mut detail) = self.commit_detail
                    && self.files_area.contains(pos)
                {
                    let content_y = self.files_area.y + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + detail.file_state.offset();
                        if idx < detail.files.len() {
                            if detail.file_state.selected() == Some(idx) {
                                open_file_diff = true;
                            } else {
                                detail.file_state.select(Some(idx));
                            }
                        }
                    }
                }
                if open_file_diff {
                    return Ok(self.try_show_commit_diff());
                }

                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_sub(1);
                        return Ok(None);
                    }
                    if self.files_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| i.saturating_sub(1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        return Ok(None);
                    }
                }
                self.select_prev();
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_add(1);
                        return Ok(None);
                    }
                    if self.files_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| (i + 1).min(detail.files.len() - 1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        return Ok(None);
                    }
                }
                self.select_next();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;

        match &self.commit_detail {
            Some(detail) if detail.diff_content.is_some() => {
                // Graph 40% | Files 25% | Diff 35%
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Percentage(25),
                        Constraint::Percentage(35),
                    ])
                    .split(area);

                self.graph_list_area = chunks[0];
                self.files_area = chunks[1];
                self.diff_area = chunks[2];

                self.draw_graph_list(frame, chunks[0]);
                // Borrow detail mutably for drawing
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1]);
                Self::draw_commit_diff(detail, frame, chunks[2]);
            }
            Some(_) => {
                // Graph 50% | Files 50%
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                self.graph_list_area = chunks[0];
                self.files_area = chunks[1];
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, chunks[0]);
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1]);
            }
            None => {
                self.graph_list_area = area;
                self.files_area = Rect::default();
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, area);
            }
        }

        Ok(())
    }
}
