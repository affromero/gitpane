use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::graph_render;

use super::*;

impl GitGraph {
    fn filter_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(branches) = &self.graph_options.filters.branches {
            parts.push(format!(
                "branches {}/{}",
                branches.len(),
                self.filter_branches.len()
            ));
        }
        if let Some(authors) = &self.graph_options.filters.authors {
            parts.push(format!(
                "authors {}/{}",
                authors.len(),
                self.filter_authors.len()
            ));
        }
        (!parts.is_empty()).then(|| format!(" [{}]", parts.join(", ")))
    }

    fn draw_graph_list(&mut self, frame: &mut Frame, area: Rect) {
        let collapsed_count = self.collapsed_branches.len();
        let mut title = match (self.graph_options.first_parent, collapsed_count) {
            (true, 0) => format!(" Git Graph — {} [1st-parent] ", self.repo_name),
            (true, n) => format!(
                " Git Graph — {} [1st-parent] ({n} collapsed) ",
                self.repo_name
            ),
            (false, 0) => format!(" Git Graph — {} ", self.repo_name),
            (false, n) => format!(" Git Graph — {} ({n} collapsed) ", self.repo_name),
        };
        if let Some(summary) = self.filter_summary() {
            title.push_str(&summary);
        }
        let t = &self.theme.graph;
        let border_color = if self.focused && self.commit_detail.is_none() {
            t.border_focused
        } else {
            t.border_unfocused
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.loading {
            let paragraph = Paragraph::new("Loading graph...")
                .style(Style::default().fg(t.loading))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if let Some(ref err) = self.error {
            let paragraph = Paragraph::new(err.as_str())
                .style(Style::default().fg(t.error_text))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if self.display_rows().is_empty() {
            let paragraph = Paragraph::new("No commits")
                .style(Style::default().fg(t.empty))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let label_max_len = self.graph_options.label_max_len;
        let max_width = area.width.saturating_sub(2) as usize; // 2 for borders
        let has_search = !self.search.input.is_empty() && !self.search.matches.is_empty();
        let items: Vec<ListItem> = self
            .display_rows()
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let dimmed = has_search && !self.search.matches.contains(&i);
                let is_collapsed = row.collapsed.is_some();
                let mut spans = graph_render::render_graph_prefix(row, t);

                if dimmed || is_collapsed {
                    for span in &mut spans {
                        span.style = Style::default().fg(t.dimmed);
                    }
                }

                if is_collapsed {
                    spans.push(Span::styled(
                        row.message.clone(),
                        Style::default()
                            .fg(t.collapsed_message)
                            .add_modifier(Modifier::ITALIC),
                    ));
                } else {
                    let id_style = if dimmed {
                        Style::default().fg(t.dimmed)
                    } else {
                        Style::default()
                            .fg(t.commit_id)
                            .add_modifier(Modifier::BOLD)
                    };
                    spans.push(Span::styled(format!("{} ", row.short_id), id_style));

                    if !dimmed {
                        spans.extend(graph_render::render_branch_labels(
                            &row.labels,
                            label_max_len,
                            t,
                        ));
                    }

                    let msg_color = if dimmed {
                        t.dimmed
                    } else if row.is_merge {
                        t.merge_message
                    } else {
                        t.commit_message
                    };
                    spans.push(Span::styled(
                        row.message.clone(),
                        Style::default().fg(msg_color),
                    ));

                    let author_color = if dimmed {
                        t.dimmed
                    } else {
                        graph_render::author_color(&row.author, t)
                    };
                    spans.push(Span::styled(
                        format!("  — {}", row.author),
                        Style::default().fg(author_color),
                    ));
                    spans.push(Span::styled(
                        format!(" {}", graph_render::format_relative_time(row.time)),
                        Style::default().fg(t.time),
                    ));

                    if let Some(ref stat) = row.diff_stat
                        && !dimmed
                    {
                        if stat.additions > 0 {
                            spans.push(Span::styled(
                                format!(" +{}", stat.additions),
                                Style::default().fg(t.addition),
                            ));
                        }
                        if stat.deletions > 0 {
                            spans.push(Span::styled(
                                format!(" -{}", stat.deletions),
                                Style::default().fg(t.deletion),
                            ));
                        }
                    }
                }

                graph_render::h_scroll_line(&mut spans, self.h_scroll, max_width);
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

    fn draw_commit_files(
        detail: &mut CommitDetail,
        frame: &mut Frame,
        area: Rect,
        theme: &crate::theme::GraphTheme,
    ) {
        let title = format!(" Files — {} ", &detail.oid[..7.min(detail.oid.len())]);

        let msg_line_count = detail.message.lines().count().max(1) as u16;
        let msg_height = (msg_line_count + 2).min(area.height / 3).clamp(3, 8);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(msg_height), Constraint::Min(3)])
            .split(area);

        detail.msg_area = chunks[0];
        detail.file_list_area = chunks[1];

        let msg_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.commit_msg_border));

        let msg_paragraph = Paragraph::new(detail.message.as_str())
            .style(Style::default().fg(theme.commit_msg_text))
            .block(msg_block)
            .wrap(Wrap { trim: false })
            .scroll((detail.msg_scroll, 0));
        frame.render_widget(msg_paragraph, chunks[0]);

        let files_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.commit_files_border));

        if detail.files.is_empty() {
            let paragraph = Paragraph::new("No files changed")
                .style(Style::default().fg(theme.commit_files_empty))
                .block(files_block);
            frame.render_widget(paragraph, chunks[1]);
            return;
        }

        let items: Vec<ListItem> = detail
            .files
            .iter()
            .map(|(status, path)| {
                let color = match status.as_str() {
                    "M" => theme.commit_files_status_modified,
                    "A" => theme.commit_files_status_added,
                    "D" => theme.commit_files_status_deleted,
                    "R" => theme.commit_files_status_renamed,
                    _ => theme.commit_files_status_other,
                };
                let spans = vec![
                    Span::styled(
                        format!(" {} ", status),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(path, Style::default().fg(theme.commit_files_path)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(files_block).highlight_style(
            Style::default()
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, chunks[1], &mut detail.file_state);
    }

    fn draw_commit_diff(
        detail: &CommitDetail,
        frame: &mut Frame,
        area: Rect,
        theme: &crate::theme::GraphTheme,
    ) {
        let Some(ref content) = detail.diff_content else {
            return;
        };

        let title = if detail.diff_focused {
            " Commit Diff (Esc to leave) "
        } else {
            " Commit Diff (Enter to scroll) "
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.commit_diff_border));

        let lines: Vec<Line> = content
            .lines()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(theme.commit_diff_added)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(theme.commit_diff_removed)
                } else if line.starts_with("@@") {
                    Style::default().fg(theme.commit_diff_hunk)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(theme.commit_diff_meta)
                } else {
                    Style::default().fg(theme.commit_diff_context)
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
            // The diff pane only takes the keys once it is focused; while it is
            // merely following the highlighted file, `j`/`k` keep walking the
            // file list.
            if detail.diff_focused {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => {
                        detail.diff_focused = false;
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
                    if std::mem::take(&mut self.needs_reload) {
                        self.reload_graph();
                    }
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
                    // The Diff pane follows the highlight, after the debounce.
                    return Ok(self.schedule_commit_diff());
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
                    return Ok(self.schedule_commit_diff());
                }
                KeyCode::Enter => {
                    // Hand the keyboard to the diff so it can be scrolled, and
                    // ask for it now rather than waiting out the debounce.
                    detail.diff_focused = true;
                    return Ok(self.try_show_commit_diff());
                }
                _ => return Ok(None),
            }
        }

        // No detail open — normal graph navigation
        match key.code {
            KeyCode::Char('n') => {
                self.search_next();
                Ok(None)
            }
            KeyCode::Char('N') => {
                self.search_prev();
                Ok(None)
            }
            KeyCode::Char('/') => {
                self.search.visible = true;
                self.search.input.clear();
                self.search.matches.clear();
                self.search.current_match = None;
                Ok(None)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.try_show_commit_files()),
            KeyCode::Char('f') => {
                self.graph_options.first_parent = !self.graph_options.first_parent;
                self.reload_graph();
                Ok(None)
            }
            KeyCode::Char('c') => {
                self.toggle_collapse_selected();
                Ok(None)
            }
            KeyCode::Char('H') => {
                self.expand_all_branches();
                Ok(None)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.h_scroll = self.h_scroll.saturating_add(4);
                Ok(None)
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.h_scroll = self.h_scroll.saturating_sub(4);
                Ok(None)
            }
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
                        if idx < self.display_rows().len() {
                            // Clicking the commit that is already open is a
                            // no-op: reloading its files would throw away the
                            // file the user picked, and a terminal double click
                            // arrives as two of these.
                            if self.is_detail_open_for(idx) {
                                return Ok(None);
                            }
                            self.state.select(Some(idx));
                            self.commit_detail = None;
                            if std::mem::take(&mut self.needs_reload) {
                                self.reload_graph();
                            }
                            // A single click selects the commit and opens its
                            // changed files (and, via CommitFilesLoaded, the
                            // highlighted file's diff), so no second click is
                            // needed to see the detail.
                            return Ok(self.try_show_commit_files());
                        }
                    }
                    return Ok(None);
                }

                // Click inside the diff hands it the keyboard, the mouse
                // counterpart of Enter on a file row.
                if let Some(ref mut detail) = self.commit_detail
                    && detail.diff_content.is_some()
                    && self.diff_area.contains(pos)
                {
                    detail.diff_focused = true;
                    return Ok(None);
                }

                // Click in commit files area (use file_list_area, not files_area)
                let mut file_highlight_moved = false;
                if let Some(ref mut detail) = self.commit_detail
                    && detail.file_list_area.contains(pos)
                {
                    let content_y = detail.file_list_area.y + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + detail.file_state.offset();
                        if idx < detail.files.len() {
                            // Highlighting a file (mouse or keys) shows its
                            // diff in the Diff pane, and returns the keyboard
                            // to the file list.
                            detail.file_state.select(Some(idx));
                            detail.diff_focused = false;
                            file_highlight_moved = true;
                        }
                    }
                }
                if file_highlight_moved {
                    return Ok(self.schedule_commit_diff());
                }

                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                let mut file_highlight_moved = false;
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_sub(1);
                        return Ok(None);
                    }
                    if detail.msg_area.contains(pos) {
                        detail.msg_scroll = detail.msg_scroll.saturating_sub(1);
                        return Ok(None);
                    }
                    if detail.file_list_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| i.saturating_sub(1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        file_highlight_moved = true;
                    }
                }
                if file_highlight_moved {
                    return Ok(self.schedule_commit_diff());
                }
                self.select_prev();
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                let mut file_highlight_moved = false;
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_add(1);
                        return Ok(None);
                    }
                    if detail.msg_area.contains(pos) {
                        detail.msg_scroll = detail.msg_scroll.saturating_add(1);
                        return Ok(None);
                    }
                    if detail.file_list_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| (i + 1).min(detail.files.len() - 1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        file_highlight_moved = true;
                    }
                }
                if file_highlight_moved {
                    return Ok(self.schedule_commit_diff());
                }
                self.select_next();
                Ok(None)
            }
            MouseEventKind::ScrollLeft => {
                self.h_scroll = self.h_scroll.saturating_sub(4);
                Ok(None)
            }
            MouseEventKind::ScrollRight => {
                self.h_scroll = self.h_scroll.saturating_add(4);
                Ok(None)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if self.graph_list_area.contains(pos) {
                    let content_y = self.graph_list_area.y + 1;
                    if mouse.row >= content_y {
                        let idx = (mouse.row - content_y) as usize + self.state.offset();
                        if idx < self.display_rows().len() {
                            self.state.select(Some(idx));
                            return Ok(Some(Action::OpenGraphContextMenu));
                        }
                    }
                }
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
                let dir = if self.horizontal_layout {
                    Direction::Vertical
                } else {
                    Direction::Horizontal
                };
                let chunks = Layout::default()
                    .direction(dir)
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
                let graph_theme = self.theme.graph.clone();
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1], &graph_theme);
                Self::draw_commit_diff(detail, frame, chunks[2], &graph_theme);
            }
            Some(_) => {
                // Graph 50% | Files 50%
                let dir = if self.horizontal_layout {
                    Direction::Vertical
                } else {
                    Direction::Horizontal
                };
                let chunks = Layout::default()
                    .direction(dir)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                self.graph_list_area = chunks[0];
                self.files_area = chunks[1];
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, chunks[0]);
                let graph_theme = self.theme.graph.clone();
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1], &graph_theme);
            }
            None => {
                self.graph_list_area = area;
                self.files_area = Rect::default();
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, area);
            }
        }

        // Search overlay at bottom of graph area
        if self.search.visible {
            let match_info = if self.search.input.is_empty() {
                String::new()
            } else {
                let current = self.search.current_match.map(|i| i + 1).unwrap_or(0);
                format!(" {}/{}", current, self.search.matches.len())
            };
            let overlay_text = format!(" / {}{} ", self.search.input, match_info);
            let overlay_area = Rect::new(
                self.graph_list_area.x,
                self.graph_list_area.y + self.graph_list_area.height.saturating_sub(1),
                self.graph_list_area
                    .width
                    .min(overlay_text.len() as u16 + 2),
                1,
            );
            let overlay = Paragraph::new(overlay_text).style(
                Style::default()
                    .fg(self.theme.graph.search_overlay_fg)
                    .bg(self.theme.graph.search_overlay_bg),
            );
            frame.render_widget(overlay, overlay_area);
        }

        Ok(())
    }
}
