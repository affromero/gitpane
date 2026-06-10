use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::status::{FileEntry, FileStatus, SubmoduleState, SubmoduleWarn};
use crate::repo_id::RepoId;
use crate::theme::{FileListTheme, Theme};

pub(crate) struct FileList {
    files: Vec<FileEntry>,
    state: ListState,
    repo_name: String,
    repo_id: Option<RepoId>,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    render_area: Rect,
    file_list_area: Rect,
    diff_area: Rect,
    // Diff view
    diff_content: Option<String>,
    diff_scroll: u16,
    pub horizontal_layout: bool,
    /// Monotonic counter to discard stale DiffLoaded results.
    diff_generation: u64,
    theme: Arc<Theme>,
}

impl FileList {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            files: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
            repo_id: None,
            focused: false,
            action_tx: None,
            render_area: Rect::default(),
            file_list_area: Rect::default(),
            diff_area: Rect::default(),
            diff_content: None,
            diff_scroll: 0,
            horizontal_layout: false,
            diff_generation: 0,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn set_files(&mut self, files: Vec<FileEntry>, repo_name: &str, repo_id: RepoId) {
        let is_same_repo = self.repo_id.as_ref() == Some(&repo_id);
        let prev_selected = self.state.selected();
        let files_changed = !is_same_repo || self.files != files;

        self.files = files;
        self.repo_name = repo_name.to_string();
        self.repo_id = Some(repo_id);

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

    pub fn diff_generation(&self) -> u64 {
        self.diff_generation
    }

    fn try_show_diff(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        let repo_id = self.repo_id.clone()?;
        let file = self.files.get(idx)?;
        self.diff_generation += 1;
        Some(Action::ShowDiff(repo_id, file.path.clone()))
    }

    fn draw_file_list(&mut self, frame: &mut Frame, area: Rect) {
        let t = &self.theme.file_list;
        let border_color = if self.focused && !self.viewing_diff() {
            t.border_focused
        } else {
            t.border_unfocused
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
                .style(Style::default().fg(t.empty_text))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|entry| {
                let color = match entry.status {
                    FileStatus::Modified => t.status_modified,
                    FileStatus::Added => t.status_added,
                    FileStatus::Deleted => t.status_deleted,
                    FileStatus::Renamed => t.status_renamed,
                    FileStatus::Untracked => t.status_untracked,
                    FileStatus::Conflicted => t.status_conflicted,
                };

                let mut spans = vec![Span::styled(
                    format!(" {} ", entry.status.label()),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )];

                if entry.is_submodule {
                    spans.extend(submodule_tag_spans(
                        &entry.submodule_state,
                        &entry.submodule_warn,
                        t,
                    ));
                }

                let path_color = if entry.is_submodule {
                    t.submodule_path
                } else {
                    t.regular_path
                };
                spans.push(Span::styled(
                    entry.path.to_string_lossy().to_string(),
                    Style::default().fg(path_color),
                ));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(t.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let Some(ref content) = self.diff_content else {
            return;
        };

        let t = &self.theme.file_list;
        let title = format!(" Diff — {} (Esc/h to close) ", self.repo_name);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.diff_border));

        let lines: Vec<Line> = content
            .lines()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(t.diff_added)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(t.diff_removed)
                } else if line.starts_with("@@") {
                    Style::default().fg(t.diff_hunk)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(t.diff_meta)
                } else {
                    Style::default().fg(t.diff_context)
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

/// Build the `[sub: …]` tag spans for a submodule file row.
/// `state` is independent from `warn` — both can be present and compose.
fn submodule_tag_spans(
    state: &Option<SubmoduleState>,
    warn: &SubmoduleWarn,
    theme: &FileListTheme,
) -> Vec<Span<'static>> {
    let bracket_style = Style::default().fg(theme.submodule_bracket);
    let unpushed_style = Style::default().fg(theme.submodule_unpushed);
    let unreach_style = Style::default().fg(theme.submodule_unreachable);

    let mut inner: Vec<Span<'static>> = Vec::new();

    let state_label = match state {
        Some(SubmoduleState::Modified) => Some("+commit"),
        Some(SubmoduleState::Uninitialized) => Some("-uninit"),
        Some(SubmoduleState::Dirty) => Some("~dirty"),
        None => None,
    };

    // Uninitialized takes precedence: never compose with warn — `sub.open()`
    // can't introspect the inner repo, so warn fields are zero anyway.
    let suppress_warn = matches!(state, Some(SubmoduleState::Uninitialized));

    if let Some(label) = state_label {
        inner.push(Span::styled(label, bracket_style));
    }

    if !suppress_warn {
        if warn.pointer_unreachable {
            if !inner.is_empty() {
                inner.push(Span::styled(" ", bracket_style));
            }
            inner.push(Span::styled("\u{26a0}unreach", unreach_style));
        } else if warn.unpushed_commits > 0 {
            if !inner.is_empty() {
                inner.push(Span::styled(" ", bracket_style));
            }
            inner.push(Span::styled(
                format!("\u{2191}{}", warn.unpushed_commits),
                unpushed_style,
            ));
        }
    }

    if inner.is_empty() {
        // No state and no warn — fall back to a plain "[submodule]" tag
        // (matches the prior behavior for submodules with no signal).
        return vec![Span::styled("[submodule] ", bracket_style)];
    }

    let mut out = Vec::with_capacity(inner.len() + 2);
    out.push(Span::styled("[sub: ", bracket_style));
    out.extend(inner);
    out.push(Span::styled("] ", bracket_style));
    out
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
            let dir = if self.horizontal_layout {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let chunks = Layout::default()
                .direction(dir)
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

#[cfg(test)]
mod tag_tests {
    use super::*;

    fn rendered(state: Option<SubmoduleState>, warn: SubmoduleWarn) -> String {
        let theme = FileListTheme::default();
        submodule_tag_spans(&state, &warn, &theme)
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn modified_clean() {
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), SubmoduleWarn::default()),
            "[sub: +commit] "
        );
    }

    #[test]
    fn modified_with_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 3,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), warn),
            "[sub: +commit \u{2191}3] "
        );
    }

    #[test]
    fn modified_with_unreachable_takes_precedence_over_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 5,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), warn),
            "[sub: +commit \u{26a0}unreach] "
        );
    }

    #[test]
    fn dirty_clean() {
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), SubmoduleWarn::default()),
            "[sub: ~dirty] "
        );
    }

    #[test]
    fn dirty_with_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 1,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), warn),
            "[sub: ~dirty \u{2191}1] "
        );
    }

    #[test]
    fn dirty_with_unreachable() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), warn),
            "[sub: ~dirty \u{26a0}unreach] "
        );
    }

    #[test]
    fn uninitialized_skips_warn() {
        // Even with warn fields set, uninitialized always renders just `-uninit`.
        let warn = SubmoduleWarn {
            unpushed_commits: 7,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Uninitialized), warn),
            "[sub: -uninit] "
        );
    }

    #[test]
    fn unreach_only_synthetic_row() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(rendered(None, warn), "[sub: \u{26a0}unreach] ");
    }

    #[test]
    fn unpushed_only_synthetic_row() {
        let warn = SubmoduleWarn {
            unpushed_commits: 4,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(rendered(None, warn), "[sub: \u{2191}4] ");
    }

    #[test]
    fn no_state_no_warn_falls_back_to_plain_tag() {
        assert_eq!(rendered(None, SubmoduleWarn::default()), "[submodule] ");
    }
}
