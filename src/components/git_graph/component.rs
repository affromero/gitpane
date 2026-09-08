use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
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
    /// The split used for this frame's detail layout: `detail_split` with the
    /// message|files border overridden by the message's line count while the
    /// user has not dragged that border (see `msg_dragged`).
    fn detail_split_with_auto_msg(&self, area: Rect, detail: &CommitDetail) -> [f64; 3] {
        let axis = if self.horizontal_layout {
            area.height
        } else {
            area.width
        };
        // Feasible integer boundary positions for this axis (short axes leave
        // no room between the stored 0.40/0.65 borders, so clamp in cells).
        let [b0, b1, b2] = detail_cell_bounds(axis, self.detail_split);
        if !self.msg_dragged {
            let line_count = detail.message.lines().count().max(1) as u16;
            let want = msg_auto_cells(line_count, axis);
            let min_cells = detail_min_cells(axis);
            // Message|files border sits `want` cells below the graph border,
            // clamped so the message and files panes each keep a minimum.
            let b1 = b0
                .saturating_add(want)
                .clamp(b0 + min_cells, b2 - min_cells);
            return [
                b0 as f64 / axis as f64,
                b1 as f64 / axis as f64,
                b2 as f64 / axis as f64,
            ];
        }
        [
            b0 as f64 / axis as f64,
            b1 as f64 / axis as f64,
            b2 as f64 / axis as f64,
        ]
    }

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
        // One clock read for the whole frame's relative-time column.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Rendered row bodies are cached per (commit, theme, width, highlight,
        // collapse) state; only the volatile tail (relative time, diff stats)
        // is rebuilt each frame. Take the cache out of `self` so the closure
        // can mutate it while `display_rows` is still borrowed.
        let mut render_cache = std::mem::take(&mut self.render_cache);
        let items: Vec<ListItem> = self
            .display_rows()
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let dimmed = has_search && !self.search.matches.contains(&i);
                let is_collapsed = row.collapsed.is_some();

                let key = RowRenderKey {
                    oid: row.oid,
                    theme_generation: self.theme_generation,
                    label_max_len,
                    dimmed,
                    collapsed: is_collapsed,
                };
                let mut spans = match render_cache.get(&key) {
                    Some(cached) => cached.clone(),
                    None => {
                        let built = graph_render::render_row_body(
                            row,
                            t,
                            label_max_len,
                            dimmed,
                            is_collapsed,
                        );
                        if render_cache.len() >= RENDER_CACHE_CAPACITY {
                            render_cache.clear();
                        }
                        render_cache.insert(key, built.clone());
                        built
                    }
                };

                spans.extend(graph_render::render_row_tail(row, t, now_secs, dimmed));

                graph_render::h_scroll_line(&mut spans, self.h_scroll, max_width);
                ListItem::new(Line::from(spans))
            })
            .collect();
        self.render_cache = render_cache;

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(t.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_commit_message(
        detail: &mut CommitDetail,
        frame: &mut Frame,
        area: Rect,
        theme: &crate::theme::GraphTheme,
    ) {
        let title = format!(" Message — {} ", &detail.oid[..7.min(detail.oid.len())]);
        detail.msg_area = area;

        let msg_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.commit_msg_border));

        let msg_paragraph = Paragraph::new(detail.message.as_str())
            .style(Style::default().fg(theme.commit_msg_text))
            .block(msg_block)
            .wrap(Wrap { trim: false })
            .scroll((detail.msg_scroll, 0));
        frame.render_widget(msg_paragraph, area);
    }

    fn draw_commit_file_list(
        detail: &mut CommitDetail,
        frame: &mut Frame,
        area: Rect,
        theme: &crate::theme::GraphTheme,
    ) {
        let title = format!(" Files — {} ", &detail.oid[..7.min(detail.oid.len())]);
        detail.file_list_area = area;

        let files_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.commit_files_border));

        if detail.files.is_empty() {
            let paragraph = Paragraph::new("No files changed")
                .style(Style::default().fg(theme.commit_files_empty))
                .block(files_block);
            frame.render_widget(paragraph, area);
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

        frame.render_stateful_widget(list, area, &mut detail.file_state);
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

/// ±2 cells hit zone for grabbing a commit-detail border.
const DETAIL_GRAB_ZONE: u16 = 2;

/// Split the commit-detail `area` into graph / message / files / diff rects
/// along the detail layout axis. `horizontal_layout` = the outer panels are
/// side by side, so the detail splits vertically (historical convention).
/// `split` holds the graph|message, message|files and files|diff fractions.
/// Each pane keeps at least `detail_min_cells(axis)` cells on the axis.
fn detail_chunks(area: Rect, split: [f64; 3], horizontal_layout: bool) -> [Rect; 4] {
    let axis = if horizontal_layout {
        area.height
    } else {
        area.width
    };
    let [b0, b1, b2] = detail_cell_bounds(axis, split);
    let make = |start: u16, len: u16, area: Rect, vertical: bool| {
        if vertical {
            Rect {
                y: area.y + start,
                height: len,
                ..area
            }
        } else {
            Rect {
                x: area.x + start,
                width: len,
                ..area
            }
        }
    };
    [
        make(0, b0, area, horizontal_layout),
        make(b0, b1 - b0, area, horizontal_layout),
        make(b1, b2 - b1, area, horizontal_layout),
        make(b2, axis - b2, area, horizontal_layout),
    ]
}

/// The minimum number of cells each detail pane keeps on the axis. Capped at
/// three, but never more than a quarter of the axis: four panes each need a
/// quarter, so a short axis (e.g. 10 cells) drops the floor to `axis / 4`
/// cells per pane, otherwise the clamp bounds below would invert.
fn detail_min_cells(axis: u16) -> u16 {
    3u16.min(axis / 4)
}

/// Feasible integer boundary positions (cells on the detail axis) for a
/// three-way split, derived from the desired fractional `split`. Each pane
/// keeps at least `detail_min_cells(axis)` cells. The clamping happens in
/// integer cell space, so float rounding can never produce an inverted
/// (min > max) range that panics the layout.
fn detail_cell_bounds(axis: u16, split: [f64; 3]) -> [u16; 3] {
    if axis == 0 {
        return [0, 0, 0];
    }
    let min_cells = detail_min_cells(axis);
    let axis_f = axis as f64;
    // Desired boundary positions (graph|message, message|files, files|diff).
    let d0 = (split[0] * axis_f).round() as u16;
    let d1 = (split[1] * axis_f).round() as u16;
    let d2 = (split[2] * axis_f).round() as u16;
    // Clamp in order; each pane keeps at least `min_cells` cells.
    let b0 = d0.clamp(min_cells, axis - 3 * min_cells);
    let b1 = d1.clamp(b0 + min_cells, axis - 2 * min_cells);
    let b2 = d2.clamp(b1 + min_cells, axis - min_cells);
    [b0, b1, b2]
}

/// Auto height (in cells) for the commit message block: follows the message's
/// line count, capped at 40% of the available axis, at least 3 cells.
fn msg_auto_cells(line_count: u16, axis: u16) -> u16 {
    (line_count + 2).min(axis * 40 / 100).max(3)
}

/// Maximum scroll offset (rows) for a wrapping paragraph inside a bordered
/// block: the content's wrapped row count (each source line wrapped at the
/// inner width using display-width word wrapping, exactly as ratatui renders
/// it) minus the rows visible between the borders. `0` when the content fits
/// or the viewport is too small to matter.
fn max_scroll_lines(text: &str, viewport_height: u16, viewport_width: u16) -> u16 {
    if viewport_height <= 2 || viewport_width <= 2 {
        return 0;
    }
    let inner = viewport_width - 2;
    // Use the same wrapping ratatui applies when rendering the paragraph
    // (display width + word boundaries), so wide (CJK) glyphs and long words
    // don't leave the content end unreachable.
    let content_rows = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(inner) as u16;
    content_rows.saturating_sub(viewport_height - 2)
}

/// Axis coordinates (row in a vertical detail layout, column in a horizontal
/// one) of the commit-detail borders: 0 = graph|message, 1 = message|files,
/// 2 = files|diff. The files|diff border is absent until a diff is loaded.
fn detail_border_positions(
    graph: Rect,
    message: Rect,
    files: Rect,
    diff: Rect,
    vertical: bool,
) -> [Option<u16>; 3] {
    let end = |r: Rect| {
        if vertical {
            r.y + r.height
        } else {
            r.x + r.width
        }
    };
    let live = |r: Rect| r.width > 0 && r.height > 0;
    let has_diff = live(diff);
    [
        live(graph).then(|| end(graph)),
        live(message).then(|| end(message)),
        if has_diff {
            live(files).then(|| end(files))
        } else {
            None
        },
    ]
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
                        let max = detail
                            .diff_content
                            .as_deref()
                            .map(|c| {
                                max_scroll_lines(c, self.diff_area.height, self.diff_area.width)
                            })
                            .unwrap_or(0);
                        detail.diff_scroll = (detail.diff_scroll + 1).min(max);
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
        let vertical = self.horizontal_layout;
        let axis_pos = if vertical { mouse.row } else { mouse.column };
        let dpos = detail_border_positions(
            self.graph_list_area,
            self.msg_area,
            self.files_area,
            self.diff_area,
            vertical,
        );
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let mut candidates = Vec::new();
                for (i, p) in dpos.iter().enumerate() {
                    if let Some(p) = *p {
                        candidates.push((i as u8, axis_pos.abs_diff(p)));
                    }
                }
                self.dragging_detail_border = candidates
                    .into_iter()
                    .filter(|(_, d)| *d <= DETAIL_GRAB_ZONE)
                    .min_by_key(|(_, d)| *d)
                    .map(|(id, _)| id);
                // Record the grab but keep processing the click, matching the
                // outer-panel border behavior: the drag engages on Drag only,
                // and a plain click near a border still reaches the content.
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

            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(border) = self.dragging_detail_border {
                    let area = self.render_area;
                    let (axis, origin) = if vertical {
                        (area.height, area.y)
                    } else {
                        (area.width, area.x)
                    };
                    let rel = axis_pos.saturating_sub(origin) as f64 / axis.max(1) as f64;
                    // Start from the split already clamped to this axis: the
                    // stored borders may be infeasible for a small area, and
                    // clamping `rel` against them could invert (min > max) and
                    // panic. Clamp in integer cell space below.
                    let [mut b0, mut b1, mut b2] = detail_cell_bounds(axis, self.detail_split);
                    let min_cells = detail_min_cells(axis);
                    // `rel` is the pointer position as a fraction of the axis;
                    // convert to a cell, then clamp onto the pane boundary.
                    let at = (rel * axis as f64).round() as u16;
                    match border {
                        0 => {
                            b0 = at.clamp(min_cells, b1 - min_cells);
                        }
                        1 => {
                            b1 = at.clamp(b0 + min_cells, b2 - min_cells);
                            self.msg_dragged = true;
                        }
                        2 => {
                            b2 = at.clamp(b1 + min_cells, axis - min_cells);
                        }
                        _ => {}
                    }
                    self.detail_split = [
                        b0 as f64 / axis as f64,
                        b1 as f64 / axis as f64,
                        b2 as f64 / axis as f64,
                    ];
                    Ok(None)
                } else {
                    Ok(None)
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging_detail_border.is_some() => {
                self.dragging_detail_border = None;
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
                        let max = detail
                            .diff_content
                            .as_deref()
                            .map(|c| {
                                max_scroll_lines(c, self.diff_area.height, self.diff_area.width)
                            })
                            .unwrap_or(0);
                        detail.diff_scroll = (detail.diff_scroll + 1).min(max);
                        return Ok(None);
                    }
                    if detail.msg_area.contains(pos) {
                        let max = max_scroll_lines(
                            &detail.message,
                            detail.msg_area.height,
                            detail.msg_area.width,
                        );
                        detail.msg_scroll = (detail.msg_scroll + 1).min(max);
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
                        // Open the context menu even when the graph is empty
                        // (e.g. a "none" branch filter): the filter controls
                        // must stay reachable, not just per-commit actions.
                        let idx = (mouse.row - content_y) as usize + self.state.offset();
                        if idx < self.display_rows().len() {
                            self.state.select(Some(idx));
                        }
                        return Ok(Some(Action::OpenGraphContextMenu));
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
                let split = self.detail_split_with_auto_msg(area, detail);
                let chunks = detail_chunks(area, split, self.horizontal_layout);
                self.graph_list_area = chunks[0];
                self.msg_area = chunks[1];
                self.files_area = chunks[2];
                self.diff_area = chunks[3];

                self.draw_graph_list(frame, chunks[0]);
                let graph_theme = self.theme.graph.clone();
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_message(detail, frame, chunks[1], &graph_theme);
                Self::draw_commit_file_list(detail, frame, chunks[2], &graph_theme);
                Self::draw_commit_diff(detail, frame, chunks[3], &graph_theme);
            }
            Some(detail) => {
                // No diff yet: graph | message | files along the same axis.
                let split = self.detail_split_with_auto_msg(area, detail);
                let mut chunks =
                    detail_chunks(area, [split[0], split[1], 1.0], self.horizontal_layout);
                chunks[3] = Rect::default();

                self.graph_list_area = chunks[0];
                self.msg_area = chunks[1];
                self.files_area = chunks[2];
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, chunks[0]);
                let graph_theme = self.theme.graph.clone();
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_message(detail, frame, chunks[1], &graph_theme);
                Self::draw_commit_file_list(detail, frame, chunks[2], &graph_theme);
            }
            None => {
                self.graph_list_area = area;
                self.msg_area = Rect::default();
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

#[cfg(test)]
mod detail_layout_tests {
    use super::{detail_border_positions, detail_chunks, max_scroll_lines, msg_auto_cells};
    use ratatui::layout::Rect;

    #[test]
    fn detail_chunks_matches_default_split() {
        let area = Rect::new(0, 0, 60, 100);
        let [g, m, f, d] = detail_chunks(area, [0.40, 0.50, 0.65], true);
        assert_eq!(g.height, 40);
        assert_eq!(m.height, 10);
        assert_eq!(f.height, 15);
        assert_eq!(d.height, 35);
        assert!(g.y < m.y && m.y < f.y && f.y < d.y);
        assert_eq!((g.y, m.y, f.y, d.y), (0, 40, 50, 65));
    }

    #[test]
    fn detail_chunks_horizontal_axis() {
        let area = Rect::new(5, 5, 100, 10);
        let [g, m, f, d] = detail_chunks(area, [0.5, 0.75, 0.9], false);
        assert_eq!(g.width, 50);
        assert_eq!(m.width, 25);
        assert_eq!(f.width, 15);
        assert_eq!(d.width, 10);
        assert_eq!((g.x, m.x, f.x, d.x), (5, 55, 80, 95));
    }

    #[test]
    fn detail_chunks_clamps_min_block() {
        let area = Rect::new(0, 0, 40, 40);
        let [g, m, f, d] = detail_chunks(area, [0.01, 0.5, 0.99], true);
        assert!(g.height >= 3 && m.height >= 3 && f.height >= 3 && d.height >= 3);
        assert_eq!(g.height + m.height + f.height + d.height, 40);
        let [g2, m2, f2, d2] = detail_chunks(area, [0.99, 0.01, 0.01], true);
        assert!(g2.height >= 3 && m2.height >= 3 && f2.height >= 3 && d2.height >= 3);
    }

    #[test]
    fn msg_auto_cells_follows_lines_with_cap() {
        assert_eq!(msg_auto_cells(1, 30), 3);
        assert_eq!(msg_auto_cells(4, 30), 6);
        assert_eq!(msg_auto_cells(50, 30), 12);
    }

    #[test]
    fn max_scroll_lines_stops_at_content_end() {
        // Fits in 10 visible rows: no scrolling allowed.
        assert_eq!(max_scroll_lines("short", 12, 80), 0);
        // 20 wrapped rows in 10 visible rows: max offset 10.
        let text = (0..20)
            .map(|_| "x".repeat(60))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(max_scroll_lines(&text, 12, 80), 10);
        // Long lines wrap; 3 source lines of 40 chars at inner width 10 -> 12 rows.
        let long = ["a".repeat(40), "b".repeat(40), "c".repeat(40)].join("\n");
        assert_eq!(max_scroll_lines(&long, 8, 12), 6); // 12 rows - 6 visible
        // Degenerate viewport.
        assert_eq!(max_scroll_lines("anything", 1, 80), 0);
        assert_eq!(max_scroll_lines("anything", 12, 1), 0);
    }

    #[test]
    fn detail_border_positions_tracks_loaded_blocks() {
        let graph = Rect::new(0, 0, 20, 40);
        let message = Rect::new(0, 40, 20, 6);
        let files = Rect::new(0, 46, 20, 19);
        let diff = Rect::new(0, 65, 20, 35);
        let [b0, b1, b2] = detail_border_positions(graph, message, files, diff, true);
        assert_eq!(b0, Some(40));
        assert_eq!(b1, Some(46));
        assert_eq!(b2, Some(65));
        // No diff: the files|diff border is absent, the message border stays.
        let [_, b1n, b2n] = detail_border_positions(graph, message, files, Rect::default(), true);
        assert_eq!(b1n, Some(46));
        assert_eq!(b2n, None);
        // message border absent when the detail was never drawn
        let [_, b1z, _] = detail_border_positions(graph, Rect::default(), files, diff, true);
        assert_eq!(b1z, None);
    }
}
