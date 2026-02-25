use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::status::{FileEntry, FileStatus};

pub(crate) struct FileList {
    files: Vec<FileEntry>,
    state: ListState,
    repo_name: String,
    repo_index: Option<usize>,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    render_area: Rect,
    file_list_area: Rect,
    diff_area: Rect,
    // Diff view
    diff_content: Option<String>,
    diff_scroll: u16,
}

impl FileList {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
            repo_index: None,
            focused: false,
            action_tx: None,
            render_area: Rect::default(),
            file_list_area: Rect::default(),
            diff_area: Rect::default(),
            diff_content: None,
            diff_scroll: 0,
        }
    }

    pub fn set_files(&mut self, files: Vec<FileEntry>, repo_name: &str, repo_index: usize) {
        let is_same_repo = self.repo_index == Some(repo_index);
        let prev_selected = self.state.selected();
        let files_changed = !is_same_repo || self.files != files;

        self.files = files;
        self.repo_name = repo_name.to_string();
        self.repo_index = Some(repo_index);

        if files_changed {
            self.diff_content = None;
            self.diff_scroll = 0;
        }

        if self.files.is_empty() {
            self.state.select(None);
        } else if is_same_repo {
            // Preserve selection on refresh
            let idx = prev_selected
                .map(|i| i.min(self.files.len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        } else {
            self.state.select(Some(0));
        }
    }

    pub fn set_diff(&mut self, content: String) {
        self.diff_content = Some(content);
        self.diff_scroll = 0;
    }

    fn select_next(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.files.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn viewing_diff(&self) -> bool {
        self.diff_content.is_some()
    }

    pub fn selected_path(&self) -> Option<String> {
        let idx = self.state.selected()?;
        let file = self.files.get(idx)?;
        Some(file.path.to_string_lossy().to_string())
    }

    fn try_show_diff(&self) -> Option<Action> {
        let idx = self.state.selected()?;
        let repo_idx = self.repo_index?;
        let file = self.files.get(idx)?;
        Some(Action::ShowDiff(repo_idx, file.path.clone()))
    }

    fn draw_file_list(&mut self, frame: &mut Frame, area: Rect) {
        let border_color = if self.focused && !self.viewing_diff() {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        let title = if self.repo_name.is_empty() {
            " Changes ".to_string()
        } else {
            format!(" Changes — {} ", self.repo_name)
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.files.is_empty() {
            let msg = if self.repo_name.is_empty() {
                "Select a repository"
            } else {
                "No changes"
            };
            let paragraph = Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|entry| {
                let color = match entry.status {
                    FileStatus::Modified => Color::Yellow,
                    FileStatus::Added => Color::Green,
                    FileStatus::Deleted => Color::Red,
                    FileStatus::Renamed => Color::Blue,
                    FileStatus::Untracked => Color::DarkGray,
                    FileStatus::Conflicted => Color::LightRed,
                };

                let spans = vec![
                    Span::styled(
                        format!(" {} ", entry.status.label()),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        entry.path.to_string_lossy().to_string(),
                        Style::default().fg(Color::White),
                    ),
                ];

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

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let Some(ref content) = self.diff_content else {
            return;
        };

        let title = format!(" Diff — {} (Esc/h to close) ", self.repo_name);
        let block = Block::default()
            .title(title)
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
            .scroll((self.diff_scroll, 0));

        frame.render_widget(paragraph, area);
    }
}

impl Component for FileList {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if self.viewing_diff() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                    self.diff_content = None;
                    self.diff_scroll = 0;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                }
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.try_show_diff()),
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);

                // In split mode, clicks in file_list_area select files
                let click_area = if self.viewing_diff() {
                    self.file_list_area
                } else {
                    self.render_area
                };

                if click_area.contains(pos) {
                    let content_y = click_area.y + 1; // +1 for border
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + self.state.offset();
                        if idx < self.files.len() {
                            // Click on already-selected row opens diff
                            if self.state.selected() == Some(idx) {
                                return Ok(self.try_show_diff());
                            }
                            self.state.select(Some(idx));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                if self.viewing_diff() {
                    let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                    if self.diff_area.contains(pos) {
                        self.diff_scroll = self.diff_scroll.saturating_sub(1);
                    } else {
                        self.select_prev();
                    }
                } else {
                    self.select_prev();
                }
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                if self.viewing_diff() {
                    let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                    if self.diff_area.contains(pos) {
                        self.diff_scroll = self.diff_scroll.saturating_add(1);
                    } else {
                        self.select_next();
                    }
                } else {
                    self.select_next();
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;

        if self.diff_content.is_some() {
            // Split: file list 40% | diff 60%
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(area);

            self.file_list_area = chunks[0];
            self.diff_area = chunks[1];

            self.draw_file_list(frame, chunks[0]);
            self.draw_diff(frame, chunks[1]);
        } else {
            self.file_list_area = area;
            self.diff_area = Rect::default();
            self.draw_file_list(frame, area);
        }

        Ok(())
    }
}
