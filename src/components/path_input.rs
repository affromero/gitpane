use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::PathBuf;
use std::sync::Arc;

use crate::action::Action;
use crate::repo_id::RepoId;
use crate::theme::Theme;

/// What the input is collecting, which determines the action emitted on Enter.
#[derive(Clone)]
enum InputPurpose {
    /// Add a repo by path (Tab completes paths).
    AddRepo,
    /// Name a branch for a new worktree of `repo` (no path completion).
    NewWorktree { repo: RepoId },
}

pub(crate) struct PathInput {
    pub visible: bool,
    input: String,
    cursor: usize,
    completions: Vec<String>,
    completion_index: Option<usize>,
    purpose: InputPurpose,
    theme: Arc<Theme>,
}

impl PathInput {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            input: String::new(),
            cursor: 0,
            completions: Vec::new(),
            completion_index: None,
            purpose: InputPurpose::AddRepo,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn show(&mut self) {
        self.reset(InputPurpose::AddRepo);
        self.visible = true;
    }

    /// Show the input to name a branch for a new worktree of `repo`.
    pub fn show_new_worktree(&mut self, repo: RepoId) {
        self.reset(InputPurpose::NewWorktree { repo });
        self.visible = true;
    }

    fn reset(&mut self, purpose: InputPurpose) {
        self.input.clear();
        self.cursor = 0;
        self.completions.clear();
        self.completion_index = None;
        self.purpose = purpose;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.reset(InputPurpose::AddRepo);
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Esc => {
                self.hide();
                Ok(None)
            }
            KeyCode::Enter => {
                if self.input.trim().is_empty() {
                    self.hide();
                    return Ok(None);
                }
                let action = match &self.purpose {
                    InputPurpose::AddRepo => Action::AddRepo(expand_tilde(&self.input)),
                    InputPurpose::NewWorktree { repo } => Action::CreateWorktree {
                        repo: repo.0.clone(),
                        branch: self.input.trim().to_string(),
                    },
                };
                self.hide();
                Ok(Some(action))
            }
            KeyCode::Tab => {
                // Path completion only makes sense when entering a path.
                if matches!(self.purpose, InputPurpose::AddRepo) {
                    self.complete_path();
                }
                Ok(None)
            }
            KeyCode::Backspace => {
                if let Some(prev) = self.input[..self.cursor].chars().next_back() {
                    self.cursor -= prev.len_utf8();
                    self.input.remove(self.cursor);
                    self.completions.clear();
                    self.completion_index = None;
                }
                Ok(None)
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor); // cursor is a char boundary
                    self.completions.clear();
                    self.completion_index = None;
                }
                Ok(None)
            }
            KeyCode::Left => {
                if let Some(prev) = self.input[..self.cursor].chars().next_back() {
                    self.cursor -= prev.len_utf8();
                }
                Ok(None)
            }
            KeyCode::Right => {
                if let Some(next) = self.input[self.cursor..].chars().next() {
                    self.cursor += next.len_utf8();
                }
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
                self.cursor += c.len_utf8();
                self.completions.clear();
                self.completion_index = None;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Insert pasted text at the cursor (bracketed paste). Newlines are dropped
    /// so a multi-line paste stays a single path/branch.
    pub fn paste(&mut self, text: &str) {
        let cleaned: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.input.insert_str(self.cursor, &cleaned);
        self.cursor += cleaned.len();
        self.completions.clear();
        self.completion_index = None;
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

        // Split on char boundaries so multibyte input (é, emoji, CJK) renders and
        // never slices mid-codepoint.
        let before_cursor = &self.input[..self.cursor];
        let cc_len = self.input[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        let cursor_char = if cc_len > 0 {
            &self.input[self.cursor..self.cursor + cc_len]
        } else {
            " "
        };
        let after_cursor = &self.input[self.cursor + cc_len..];

        let (prompt, title) = match self.purpose {
            InputPurpose::AddRepo => (
                " Add repo: ",
                " Path (Tab: complete, Enter: add, Esc: cancel) ",
            ),
            InputPurpose::NewWorktree { .. } => (
                " New worktree branch: ",
                " Branch (Enter: create, Esc: cancel) ",
            ),
        };

        let t = &self.theme.overlay;
        let mut spans = vec![
            Span::styled(
                prompt,
                Style::default()
                    .fg(t.path_input_prompt)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(before_cursor),
            Span::styled(
                cursor_char,
                Style::default()
                    .bg(t.path_input_caret_bg)
                    .fg(t.path_input_caret_fg),
            ),
            Span::raw(after_cursor),
        ];

        if let Some(idx) = self.completion_index {
            spans.push(Span::styled(
                format!("  ({}/{})", idx + 1, self.completions.len()),
                Style::default().fg(t.path_input_hint),
            ));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.path_input_border))
            .title(title);

        let paragraph = Paragraph::new(Line::from(spans)).block(block);
        frame.render_widget(paragraph, input_area);
    }
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if input == "~" {
            return home;
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    // Leave `~other` (a literal name or `~user`) untouched — only bare `~`/`~/`
    // expand to $HOME.
    PathBuf::from(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn input() -> PathInput {
        PathInput::new(std::sync::Arc::new(Theme::default()))
    }
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn press(p: &mut PathInput, code: KeyCode) {
        let _ = p.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn multibyte_typing_keeps_cursor_on_char_boundary() {
        let mut p = input();
        for c in "é🚀ü/r".chars() {
            let _ = p.handle_key_event(key(c));
            assert!(p.input.is_char_boundary(p.cursor));
        }
        assert_eq!(p.input, "é🚀ü/r");
        // Backspace removes whole codepoints and never lands mid-codepoint.
        for _ in 0..5 {
            press(&mut p, KeyCode::Backspace);
            assert!(p.input.is_char_boundary(p.cursor));
        }
        assert_eq!(p.input, "");
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn left_right_move_by_codepoint() {
        let mut p = input();
        for c in "aé🚀".chars() {
            let _ = p.handle_key_event(key(c));
        }
        press(&mut p, KeyCode::Left); // before 🚀
        press(&mut p, KeyCode::Left); // before é
        assert_eq!(p.cursor, 1); // just after the 1-byte 'a'
        press(&mut p, KeyCode::Right); // past é
        assert_eq!(p.cursor, 1 + "é".len());
        assert!(p.input.is_char_boundary(p.cursor));
    }

    #[test]
    fn paste_inserts_and_strips_newlines() {
        let mut p = input();
        p.paste("/some/path\nwith-newline\r");
        assert_eq!(p.input, "/some/pathwith-newline");
        assert!(p.input.is_char_boundary(p.cursor));
    }

    #[test]
    fn expand_tilde_only_bare_tilde_and_slash() {
        let home = dirs::home_dir().expect("home dir in test env");
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Code"), home.join("Code"));
        // `~other` must stay literal, not become $HOME/other.
        assert_eq!(expand_tilde("~other"), PathBuf::from("~other"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }
}
