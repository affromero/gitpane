use color_eyre::Result;
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::action::Action;
use crate::components::Component;
use crate::git::status::{FileEntry, FileStatus};

pub(crate) struct FileList {
    files: Vec<FileEntry>,
    state: ListState,
    repo_name: String,
}

impl FileList {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
        }
    }

    pub fn set_files(&mut self, files: Vec<FileEntry>, repo_name: &str) {
        self.files = files;
        self.repo_name = repo_name.to_string();
        if self.files.is_empty() {
            self.state.select(None);
        } else {
            self.state.select(Some(0));
        }
    }

    #[allow(dead_code)]
    pub fn select_next(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.files.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    #[allow(dead_code)]
    pub fn select_prev(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }
}

impl Component for FileList {
    fn handle_key_event(&mut self, _key: KeyEvent) -> Result<Option<Action>> {
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let title = if self.repo_name.is_empty() {
            " Changes ".to_string()
        } else {
            format!(" Changes — {} ", self.repo_name)
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

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
            return Ok(());
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
        Ok(())
    }
}
