use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;

use super::*;

use super::render::{
    DETAIL_GRAB_ZONE, detail_border_positions, detail_cell_bounds, detail_chunks, detail_min_cells,
    max_scroll_lines,
};

impl Component for GitGraph {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if self.file_search_active() {
            return Ok(self.handle_file_search_key(key));
        }
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
                KeyCode::Char('/') => {
                    self.start_file_search();
                    return Ok(None);
                }
                KeyCode::Esc => {
                    self.commit_detail = None;
                    if std::mem::take(&mut self.needs_reload) {
                        self.reload_graph();
                    }
                    return Ok(None);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if !detail.files.is_empty() {
                        detail.step_file(1);
                    }
                    // The Diff pane follows the highlight, after the debounce.
                    return Ok(self.file_selection_changed());
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !detail.files.is_empty() {
                        detail.step_file(-1);
                    }
                    return Ok(self.file_selection_changed());
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
                            if !detail.file_matches(idx) {
                                return Ok(None);
                            }
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
                    return Ok(self.file_selection_changed());
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
                        detail.step_file(-1);
                        file_highlight_moved = true;
                    }
                }
                if file_highlight_moved {
                    return Ok(self.file_selection_changed());
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
                        detail.step_file(1);
                        file_highlight_moved = true;
                    }
                }
                if file_highlight_moved {
                    return Ok(self.file_selection_changed());
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
    use super::super::render::msg_auto_cells;
    use super::{detail_border_positions, detail_chunks, max_scroll_lines};
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
