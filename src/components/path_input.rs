use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::PathBuf;

use crate::action::Action;

pub(crate) struct PathInput {
    pub visible: bool,
    input: String,
    cursor: usize,
    completions: Vec<String>,
    completion_index: Option<usize>,
}

impl PathInput {
    pub fn new() -> Self {
        Self {
            visible: false,
            input: String::new(),
            cursor: 0,
            completions: Vec::new(),
            completion_index: None,
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
        self.input.clear();
        self.cursor = 0;
        self.completions.clear();
        self.completion_index = None;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.input.clear();
        self.cursor = 0;
        self.completions.clear();
        self.completion_index = None;
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Esc => {
                self.hide();
                Ok(None)
            }
            KeyCode::Enter => {
                if self.input.is_empty() {
                    self.hide();
                    return Ok(None);
                }
                let path = expand_tilde(&self.input);
                self.hide();
                Ok(Some(Action::AddRepo(path)))
            }
            KeyCode::Tab => {
                self.complete_path();
                Ok(None)
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                    self.completions.clear();
                    self.completion_index = None;
                }
                Ok(None)
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                    self.completions.clear();
                    self.completion_index = None;
                }
                Ok(None)
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Ok(None)
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.input.len());
                Ok(None)
            }
            KeyCode::Home | KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = 0;
                Ok(None)
            }
            KeyCode::End | KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.input.len();
                Ok(None)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
                self.completions.clear();
                self.completion_index = None;
                Ok(None)
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
                self.completions.clear();
                self.completion_index = None;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn complete_path(&mut self) {
        // If cycling through existing completions
        if !self.completions.is_empty() {
            let next = match self.completion_index {
                Some(i) => (i + 1) % self.completions.len(),
                None => 0,
            };
            self.completion_index = Some(next);
            self.input = self.completions[next].clone();
            self.cursor = self.input.len();
            return;
        }

        // Build completions from filesystem
        let expanded = expand_tilde(&self.input);
        let (dir, prefix) = if expanded.is_dir() && self.input.ends_with('/') {
            (expanded.clone(), String::new())
        } else {
            let parent = expanded
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            let prefix = expanded
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (parent, prefix)
        };

        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };

        let mut matches: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && !prefix.starts_with('.') {
                    return None;
                }
                if name.starts_with(&prefix) {
                    // Reconstruct the user-facing path
                    let full = if self.input.ends_with('/') || prefix.is_empty() {
                        format!("{}{}/", self.input, name)
                    } else {
                        let base = self.input.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
                        if base.is_empty() {
                            format!("{}/", name)
                        } else {
                            format!("{}/{}/", base, name)
                        }
                    };
                    Some(full)
                } else {
                    None
                }
            })
            .collect();

        matches.sort();

        if matches.len() == 1 {
            self.input = matches[0].clone();
            self.cursor = self.input.len();
        } else if !matches.is_empty() {
            self.completions = matches;
            self.completion_index = Some(0);
            self.input = self.completions[0].clone();
            self.cursor = self.input.len();
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        // Single-line input bar near the bottom
        let input_area = Rect::new(area.x, area.height.saturating_sub(3), area.width, 3);

        frame.render_widget(Clear, input_area);

        let before_cursor = &self.input[..self.cursor];
        let cursor_char = self.input.get(self.cursor..self.cursor + 1).unwrap_or(" ");
        let after_cursor = if self.cursor < self.input.len() {
            &self.input[self.cursor + 1..]
        } else {
            ""
        };

        let mut spans = vec![
            Span::styled(
                " Add repo: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(before_cursor),
            Span::styled(
                cursor_char,
                Style::default().bg(Color::White).fg(Color::Black),
            ),
            Span::raw(after_cursor),
        ];

        if let Some(idx) = self.completion_index {
            spans.push(Span::styled(
                format!("  ({}/{})", idx + 1, self.completions.len()),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Path (Tab: complete, Enter: add, Esc: cancel) ");

        let paragraph = Paragraph::new(Line::from(spans)).block(block);
        frame.render_widget(paragraph, input_area);
    }
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~')
        && let Some(home) = dirs::home_dir()
    {
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        return home.join(rest);
    }
    PathBuf::from(input)
}
