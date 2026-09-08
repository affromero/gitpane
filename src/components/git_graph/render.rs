use crate::git::graph_render;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use super::*;

impl GitGraph {
    /// The split used for this frame's detail layout: `detail_split` with the
    /// message|files border overridden by the message's line count while the
    /// user has not dragged that border (see `msg_dragged`).
    pub(super) fn detail_split_with_auto_msg(&self, area: Rect, detail: &CommitDetail) -> [f64; 3] {
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

    pub(super) fn filter_summary(&self) -> Option<String> {
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

    pub(super) fn draw_graph_list(&mut self, frame: &mut Frame, area: Rect) {
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

    pub(super) fn draw_commit_message(
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

    pub(super) fn draw_commit_file_list(
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

    pub(super) fn draw_commit_diff(
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
pub(super) const DETAIL_GRAB_ZONE: u16 = 2;

/// Split the commit-detail `area` into graph / message / files / diff rects
/// along the detail layout axis. `horizontal_layout` = the outer panels are
/// side by side, so the detail splits vertically (historical convention).
/// `split` holds the graph|message, message|files and files|diff fractions.
/// Each pane keeps at least `detail_min_cells(axis)` cells on the axis.
pub(super) fn detail_chunks(area: Rect, split: [f64; 3], horizontal_layout: bool) -> [Rect; 4] {
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
pub(super) fn detail_min_cells(axis: u16) -> u16 {
    3u16.min(axis / 4)
}

/// Feasible integer boundary positions (cells on the detail axis) for a
/// three-way split, derived from the desired fractional `split`. Each pane
/// keeps at least `detail_min_cells(axis)` cells. The clamping happens in
/// integer cell space, so float rounding can never produce an inverted
/// (min > max) range that panics the layout.
pub(super) fn detail_cell_bounds(axis: u16, split: [f64; 3]) -> [u16; 3] {
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
pub(super) fn msg_auto_cells(line_count: u16, axis: u16) -> u16 {
    (line_count + 2).min(axis * 40 / 100).max(3)
}

/// Maximum scroll offset (rows) for a wrapping paragraph inside a bordered
/// block: the content's wrapped row count (each source line wrapped at the
/// inner width using display-width word wrapping, exactly as ratatui renders
/// it) minus the rows visible between the borders. `0` when the content fits
/// or the viewport is too small to matter.
pub(super) fn max_scroll_lines(text: &str, viewport_height: u16, viewport_width: u16) -> u16 {
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
pub(super) fn detail_border_positions(
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
