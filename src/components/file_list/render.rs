use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::components::scroll_pane::BarColors;

use super::*;

impl FileList {
    pub(super) fn draw_file_list(&mut self, frame: &mut Frame, area: Rect) {
        let t = &self.theme.file_list;
        let border_color = if self.focused && !self.viewing_diff() {
            t.border_focused
        } else {
            t.border_unfocused
        };

        let mut title = match self.source.as_ref() {
            None => " Changes ".to_string(),
            Some(FileSource::WorkingTree { name, .. }) => format!(" Changes — {name} "),
            Some(FileSource::Stash(stash)) => format!(" Stash — {} ", stash.title()),
        };
        if self.searching || self.filter.is_active() {
            title = format!("{title} /{}▏ ", self.filter.query());
        }

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.files.is_empty() {
            let (msg, style) = match &self.content_state {
                ContentState::Loading => ("Loading stash…".to_string(), t.empty_text),
                ContentState::Error(error) => (error.clone(), t.status_deleted),
                ContentState::Ready => match self.source.as_ref() {
                    None => ("Select a repository".to_string(), t.empty_text),
                    Some(FileSource::Stash(_)) => {
                        ("No saved changes in this stash".to_string(), t.empty_text)
                    }
                    Some(FileSource::WorkingTree { .. }) => {
                        ("No changes".to_string(), t.empty_text)
                    }
                },
            };
            let paragraph = Paragraph::new(msg)
                .style(Style::default().fg(style))
                .block(block)
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = self
            .files
            .iter()
            .enumerate()
            .map(|(file_index, entry)| {
                let mut item_style = Style::default();
                if self.filter.is_active() && self.filter.position_of(file_index).is_some() {
                    item_style = item_style.add_modifier(Modifier::BOLD);
                }
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
                        &entry.submodule_head,
                        &entry.submodule_warn,
                        t,
                    ));
                }

                if let Some(stash_file) = self
                    .stash_files
                    .as_ref()
                    .and_then(|files| files.get(file_index))
                {
                    if stash_file.source == crate::git::stash::StashSource::SavedUntracked {
                        spans.push(Span::styled(
                            " [saved untracked] ",
                            Style::default().fg(t.status_untracked),
                        ));
                    }
                    if stash_file.is_binary {
                        spans.push(Span::styled(
                            " [binary] ",
                            Style::default().fg(t.status_modified),
                        ));
                    }
                }

                let path_color = if entry.is_submodule {
                    t.submodule_path
                } else {
                    t.regular_path
                };
                let path_str = &self.path_strings[file_index];
                spans.extend(crate::components::highlight_matches(
                    path_str,
                    self.filter.query(),
                    Style::default().fg(path_color),
                    Style::default()
                        .fg(t.border_focused)
                        .add_modifier(Modifier::BOLD),
                ));

                ListItem::new(Line::from(spans)).style(item_style)
            })
            .collect();

        // No-matches note when the filter hides everything (very large lists).
        let list = if self.filter.is_active() && self.filter.count() == 0 {
            let mut items = items;
            items.push(ListItem::new(Line::from(Span::styled(
                "  no matching files ",
                Style::default().fg(t.empty_text),
            ))));
            List::new(items).block(block)
        } else {
            List::new(items).block(block)
        };
        let list = list.highlight_style(
            Style::default()
                .bg(t.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);

        // Bottom input line: makes the typing mode unmistakable even while the
        // query is still empty (it would otherwise look like nothing happened).
        if self.searching || self.filter.is_active() {
            let input = format!(" /{}▏ ", self.filter.query());
            let over = Rect::new(
                area.x + 1,
                area.y + area.height.saturating_sub(1),
                (area.width.saturating_sub(2)).min(input.chars().count() as u16),
                1,
            );
            frame.render_widget(
                Paragraph::new(input)
                    .style(Style::default().fg(t.border_focused).bg(t.selection_bg)),
                over,
            );
        }
    }
    pub(super) fn draw_diff(&mut self, frame: &mut Frame, area: Rect) {
        // `draw` stored this same rect in `self.diff_area`, so every consumer's
        // cache key stays one width.
        let pane = area;
        if !self.ensure_diff_rows(pane) {
            return;
        }
        let Some(ref content) = self.diff_content else {
            return;
        };
        let Some(layout) = self.diff_scroll_layout_for(pane, self.diff_scroll) else {
            return;
        };
        let Some((_, _, rows)) = self.diff_rows.as_ref() else {
            return;
        };

        let t = &self.theme.file_list;
        let title = match self.source.as_ref() {
            Some(FileSource::Stash(stash)) => format!(
                " Diff — {} · {} (Esc/h to close) ",
                stash.repo_name, stash.label
            ),
            Some(FileSource::WorkingTree { name, .. }) => {
                format!(" Diff — {name} (Esc/h to close) ")
            }
            None => " Diff (Esc/h to close) ".to_string(),
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.diff_border));

        // Only the viewport's rows are built and wrapped (the index locates the
        // window by byte range), so a long diff scrolls as fast at the bottom
        // as at the top.
        let lines = scroll_pane::window_lines(
            content,
            rows.index(),
            layout.gauge.offset(),
            usize::from(layout.content.height),
            |line| diff_line_style(line, t),
        );
        let gauge = scroll_pane::render_window_pane(
            frame,
            pane,
            block,
            lines,
            layout,
            BarColors {
                thumb: t.diff_scrollbar_thumb,
                track: t.diff_scrollbar_track,
            },
        );
        self.diff_scroll = gauge.offset();
    }
}

/// The style of one diff line, from its prefix: added, removed, hunk header,
/// file metadata, or context.
fn diff_line_style(line: &str, t: &FileListTheme) -> Style {
    if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(t.diff_added)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(t.diff_removed)
    } else if line.starts_with("@@") {
        Style::default().fg(t.diff_hunk)
    } else if line.starts_with("diff ") || line.starts_with("index ") {
        Style::default().fg(t.diff_meta)
    } else {
        Style::default().fg(t.diff_context)
    }
}

/// Build the `[sub: …]` tag spans for a submodule file row. `state`, `head`,
/// and `warn` are independent and compose, e.g. `[sub: +commit @feature ↑3 ↛main]`.
pub(super) fn submodule_tag_spans(
    state: &Option<SubmoduleState>,
    head: &Option<SubmoduleHead>,
    warn: &SubmoduleWarn,
    theme: &FileListTheme,
) -> Vec<Span<'static>> {
    let bracket_style = Style::default().fg(theme.submodule_bracket);
    let branch_style = Style::default().fg(theme.submodule_branch);
    let unpushed_style = Style::default().fg(theme.submodule_unpushed);
    let unreach_style = Style::default().fg(theme.submodule_unreachable);
    let merge_style = Style::default().fg(theme.submodule_needs_merge);

    let mut inner: Vec<Span<'static>> = Vec::new();

    let state_label = match state {
        Some(SubmoduleState::Modified) => Some("+commit"),
        Some(SubmoduleState::Uninitialized) => Some("-uninit"),
        Some(SubmoduleState::Dirty) => Some("~dirty"),
        None => None,
    };

    // Uninitialized takes precedence: never compose with branch/warn —
    // `sub.open()` can't introspect the inner repo, so those fields are empty.
    let suppress_warn = matches!(state, Some(SubmoduleState::Uninitialized));

    if let Some(label) = state_label {
        inner.push(Span::styled(label, bracket_style));
    }

    // Push a separating space when the tag already has content.
    fn sep(inner: &mut Vec<Span<'static>>, style: Style) {
        if !inner.is_empty() {
            inner.push(Span::styled(" ", style));
        }
    }

    if !suppress_warn {
        // Branch indicator: `@feature`, or `@detached` for a detached HEAD.
        if let Some(h) = head {
            let label = match h {
                SubmoduleHead::Branch(name) => format!("@{name}"),
                SubmoduleHead::Detached => "@detached".to_string(),
            };
            sep(&mut inner, bracket_style);
            inner.push(Span::styled(label, branch_style));
        }

        // Push/merge warnings. `⚠unreach` (on no remote) dominates; otherwise
        // `↑N` (ahead of upstream) and `↛main` (not on the default branch)
        // compose additively.
        if warn.pointer_unreachable {
            sep(&mut inner, bracket_style);
            inner.push(Span::styled("\u{26a0}unreach", unreach_style));
        } else {
            if warn.unpushed_commits > 0 {
                sep(&mut inner, bracket_style);
                inner.push(Span::styled(
                    format!("\u{2191}{}", warn.unpushed_commits),
                    unpushed_style,
                ));
            }
            if warn.needs_merge_to_default {
                sep(&mut inner, bracket_style);
                inner.push(Span::styled("\u{219b}main", merge_style));
            }
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
