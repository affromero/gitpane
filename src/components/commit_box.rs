//! Commit message box: a summary line and an optional description for the
//! checkout the Changes panel shows. `c` opens it.

use std::collections::HashMap;
use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use crate::action::Action;
use crate::repo_id::RepoId;
use crate::theme::Theme;

/// Longer summaries get cut off in `git log --oneline` and most Git UIs.
const SUMMARY_LIMIT: usize = 72;
/// Rows a failure may take before the rest of Git's output is cut.
const ERROR_ROWS: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Summary,
    Description,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Committing,
    Drafting,
}

struct Target {
    id: RepoId,
    name: String,
    staged: usize,
}

/// Text left in a checkout's box when it closed without committing.
struct Draft {
    summary: String,
    description: Vec<String>,
}

pub(crate) struct CommitBox {
    pub visible: bool,
    target: Option<Target>,
    summary: TextArea<'static>,
    description: TextArea<'static>,
    focus: Field,
    busy: Option<Busy>,
    error: Option<String>,
    can_draft: bool,
    drafts: HashMap<RepoId, Draft>,
    theme: Arc<Theme>,
}

impl CommitBox {
    pub fn new(theme: Arc<Theme>) -> Self {
        let mut commit_box = Self {
            visible: false,
            target: None,
            summary: TextArea::default(),
            description: TextArea::default(),
            focus: Field::Summary,
            busy: None,
            error: None,
            can_draft: false,
            drafts: HashMap::new(),
            theme,
        };
        commit_box.restyle();
        commit_box
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
        self.restyle();
    }

    /// Open for `id` with the text it had when its box last closed.
    pub fn show(&mut self, id: RepoId, name: &str, staged: usize, can_draft: bool) {
        let draft = self.drafts.remove(&id);
        let (summary, description) = draft
            .map(|draft| (draft.summary, draft.description))
            .unwrap_or_default();
        self.set_text(summary, description);
        self.target = Some(Target {
            id,
            name: name.to_string(),
            staged,
        });
        self.focus = Field::Summary;
        self.busy = None;
        self.error = None;
        self.can_draft = can_draft;
        self.visible = true;
        self.restyle();
    }

    /// Close, keeping the text for the next time this checkout's box opens.
    pub fn hide(&mut self) {
        if let Some(target) = self.target.take() {
            let summary = self.summary_text();
            let description = self.description.lines().to_vec();
            if !summary.is_empty() || description.iter().any(|line| !line.trim().is_empty()) {
                self.drafts.insert(
                    target.id,
                    Draft {
                        summary,
                        description,
                    },
                );
            }
        }
        self.visible = false;
        self.busy = None;
        self.error = None;
    }

    pub fn target_id(&self) -> Option<&RepoId> {
        self.target.as_ref().map(|target| &target.id)
    }

    /// The commit is running; input waits until it reports back.
    pub fn set_committing(&mut self) {
        self.busy = Some(Busy::Committing);
        self.error = None;
    }

    /// `id`'s commit landed: drop its text and close if it is showing.
    pub fn committed(&mut self, id: &RepoId) {
        self.drafts.remove(id);
        if self.target_id() == Some(id) {
            self.target = None;
            self.set_text(String::new(), Vec::new());
            self.hide();
            self.restyle();
        }
    }

    /// Show why `id`'s commit or draft failed, keeping the text. Returns false
    /// when the box is not showing `id`, so the caller reports it elsewhere.
    pub fn failed(&mut self, id: &RepoId, error: &str) -> bool {
        if !self.visible || self.target_id() != Some(id) {
            return false;
        }
        self.busy = None;
        self.error = Some(error.trim().to_string());
        true
    }

    /// Put a drafted message in `id`'s box: the first line is the summary, the
    /// rest the description. A closed box gets it the next time it opens.
    pub fn fill(&mut self, id: &RepoId, message: &str) {
        let mut lines = message.lines();
        let summary = lines.next().unwrap_or_default().trim().to_string();
        let description: Vec<String> = lines
            .skip_while(|line| line.trim().is_empty())
            .map(str::to_string)
            .collect();
        if self.visible && self.target_id() == Some(id) {
            self.set_text(summary, description);
            self.busy = None;
            self.error = None;
            self.restyle();
        } else {
            self.drafts.insert(
                id.clone(),
                Draft {
                    summary,
                    description,
                },
            );
        }
    }

    /// Bracketed paste. In the summary, everything after the first line goes
    /// to the description.
    pub fn paste(&mut self, text: &str) {
        if self.busy.is_some() {
            return;
        }
        self.error = None;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.focus {
            Field::Summary => {
                let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
                self.summary.insert_str(first);
                let rest = rest.trim_start_matches('\n');
                if !rest.is_empty() {
                    self.description.insert_str(rest);
                    self.focus = Field::Description;
                    self.restyle();
                }
            }
            Field::Description => {
                self.description.insert_str(&text);
            }
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if key.code == KeyCode::Esc {
            self.hide();
            return Ok(None);
        }
        if self.busy.is_some() {
            return Ok(None);
        }
        self.error = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => return Ok(self.submit()),
            KeyCode::Char('g') if ctrl => return Ok(self.request_draft()),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Field::Summary => Field::Description,
                    Field::Description => Field::Summary,
                };
                self.restyle();
                return Ok(None);
            }
            KeyCode::Enter if self.focus == Field::Summary => return Ok(self.submit()),
            // The text area turns Ctrl+M into a newline; the summary is one line.
            KeyCode::Char('m') if ctrl && self.focus == Field::Summary => return Ok(None),
            _ => {}
        }
        match self.focus {
            Field::Summary => self.summary.input(key),
            Field::Description => self.description.input(key),
        };
        Ok(None)
    }

    fn submit(&mut self) -> Option<Action> {
        let id = self.target.as_ref()?.id.clone();
        let summary = self.summary_text();
        if summary.is_empty() {
            self.error = Some("Write a summary first".to_string());
            return None;
        }
        let description = self.description.lines().join("\n");
        let description = description.trim_matches('\n').trim_end();
        let message = if description.trim().is_empty() {
            summary
        } else {
            format!("{summary}\n\n{description}")
        };
        Some(Action::Commit {
            id,
            message,
            stage_all: false,
        })
    }

    fn request_draft(&mut self) -> Option<Action> {
        let id = self.target.as_ref()?.id.clone();
        if !self.can_draft {
            self.error = Some("Set [commit] draft_command to draft messages".to_string());
            return None;
        }
        self.busy = Some(Busy::Drafting);
        Some(Action::DraftCommitMessage(id))
    }

    fn summary_text(&self) -> String {
        self.summary.lines().join(" ").trim().to_string()
    }

    fn set_text(&mut self, summary: String, description: Vec<String>) {
        self.summary = TextArea::new(vec![summary]);
        self.summary.move_cursor(CursorMove::End);
        let description = if description.is_empty() {
            vec![String::new()]
        } else {
            description
        };
        self.description = TextArea::new(description);
        self.description.move_cursor(CursorMove::Bottom);
        self.description.move_cursor(CursorMove::End);
    }

    /// Theme colors, and a caret only in the focused field.
    fn restyle(&mut self) {
        let t = &self.theme.overlay;
        let caret = Style::default()
            .bg(t.path_input_caret_bg)
            .fg(t.path_input_caret_fg);
        let hint = Style::default().fg(t.path_input_hint);
        let focus = self.focus;
        for (area, field, placeholder) in [
            (&mut self.summary, Field::Summary, "Summary"),
            (
                &mut self.description,
                Field::Description,
                "Description (optional)",
            ),
        ] {
            area.set_cursor_line_style(Style::default());
            area.set_cursor_style(if focus == field {
                caret
            } else {
                Style::default()
            });
            area.set_placeholder_text(placeholder);
            area.set_placeholder_style(hint);
        }
        self.description.set_wrap_mode(WrapMode::WordOrGlyph);
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (true, Some(target)) = (self.visible, self.target.as_ref()) else {
            return;
        };
        let t = &self.theme.overlay;
        let error_color = self.theme.status_bar.error_text;
        let width = area.width.saturating_sub(4).min(80);
        let height = area.height.saturating_sub(2).min(18);
        let [vert] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vert);
        frame.render_widget(Clear, rect);

        let staged = match target.staged {
            0 => "nothing staged".to_string(),
            1 => "1 staged file".to_string(),
            n => format!("{n} staged files"),
        };
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if self.error.is_some() {
                error_color
            } else {
                t.path_input_border
            }))
            .title(format!(" Commit {} · {staged} ", target.name));
        let inner = outer.inner(rect);
        frame.render_widget(outer, rect);

        let status = self.status_lines();
        let status_rows = (status.len() as u16).clamp(1, ERROR_ROWS);
        let [summary_area, description_area, status_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(status_rows),
        ])
        .areas(inner);

        let count = self.summary_text().chars().count();
        let count_color = if count > SUMMARY_LIMIT {
            error_color
        } else {
            t.path_input_hint
        };
        let summary_block = self.field_block(Field::Summary, " Summary ").title(
            Line::from(Span::styled(
                format!(" {count}/{SUMMARY_LIMIT} "),
                Style::default().fg(count_color),
            ))
            .right_aligned(),
        );
        frame.render_widget(&self.summary, summary_block.inner(summary_area));
        frame.render_widget(summary_block, summary_area);

        let description_block = self.field_block(Field::Description, " Description ");
        frame.render_widget(&self.description, description_block.inner(description_area));
        frame.render_widget(description_block, description_area);

        frame.render_widget(
            Paragraph::new(status).wrap(Wrap { trim: false }),
            status_area,
        );
    }

    fn field_block(&self, field: Field, title: &'static str) -> Block<'static> {
        let t = &self.theme.overlay;
        let style = if self.focus == field {
            Style::default()
                .fg(t.path_input_border)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.path_input_hint)
        };
        Block::default()
            .borders(Borders::ALL)
            .border_style(style)
            .title(Span::styled(title, style))
    }

    /// Progress, the failure, or the keys for the focused field.
    fn status_lines(&self) -> Vec<Line<'static>> {
        let t = &self.theme.overlay;
        let hint = Style::default().fg(t.path_input_hint);
        if let Some(busy) = self.busy {
            let text = match busy {
                Busy::Committing => "Committing…",
                Busy::Drafting => "Drafting a message…",
            };
            return vec![Line::from(Span::styled(text, hint))];
        }
        if let Some(error) = &self.error {
            let style = Style::default()
                .fg(self.theme.status_bar.error_text)
                .add_modifier(Modifier::BOLD);
            return error
                .lines()
                .filter(|line| !line.trim().is_empty())
                .enumerate()
                .map(|(i, line)| {
                    let prefix = if i == 0 { "✗ " } else { "  " };
                    Line::from(Span::styled(format!("{prefix}{line}"), style))
                })
                .collect();
        }
        let key = |k: &'static str| {
            Span::styled(
                k,
                Style::default()
                    .fg(t.path_input_prompt)
                    .add_modifier(Modifier::BOLD),
            )
        };
        let desc = |d: &'static str| Span::styled(d, hint);
        let mut spans = match self.focus {
            Field::Summary => vec![
                key("Enter"),
                desc(" commit · "),
                key("Tab"),
                desc(" description · "),
            ],
            Field::Description => vec![
                key("Ctrl+S"),
                desc(" commit · "),
                key("Tab"),
                desc(" summary · "),
            ],
        };
        if self.can_draft {
            spans.extend([key("Ctrl+G"), desc(" draft · ")]);
        }
        spans.extend([key("Esc"), desc(" close")]);
        vec![Line::from(spans)]
    }
}

#[cfg(test)]
mod tests;
