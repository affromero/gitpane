use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;

use super::*;

use super::render::{
    DETAIL_GRAB_ZONE, detail_border_positions, detail_cell_bounds, detail_chunks, detail_min_cells,
};
use crate::components::scroll_pane::{self, ScrollLayout};

/// One `j`/`k` or wheel step of the commit diff, clamped to what the pane as
/// last drawn can show (its own column is reserved by the scroll indicator, so
/// the wrap width — and therefore the last screenful — comes from `pane`).
/// The clamp reads the memoized row index, so a large diff is wrapped once per
/// (content, width), not once per step.
fn step_diff_scroll(detail: &mut CommitDetail, pane: Rect, delta: i16) {
    let scroll = detail.diff_scroll;
    if !detail.ensure_diff_rows(pane) {
        detail.diff_scroll = 0;
        return;
    }
    detail.diff_scroll = detail
        .diff_scroll_layout_for(pane, scroll.saturating_add_signed(delta))
        .map(|layout| layout.gauge.offset())
        .unwrap_or(0);
}

/// One `j`/`k` or wheel step of the commit message pane, clamped the same way.
fn step_msg_scroll(detail: &mut CommitDetail, pane: Rect, delta: i16) {
    let scroll = detail.msg_scroll;
    detail.ensure_msg_rows(pane);
    detail.msg_scroll = detail
        .msg_scroll_layout_for(pane, scroll.saturating_add_signed(delta))
        .map(|layout| layout.gauge.offset())
        .unwrap_or(0);
}

impl GitGraph {
    /// The scroll layout of one commit-detail pane, as the last frame drew it.
    /// Offsets do not enter the layout that a scrub needs (the column and its
    /// extent depend only on the content and the pane), so `0` is passed through.
    fn pane_layout(&mut self, pane: DetailPane) -> Option<ScrollLayout> {
        let detail = self.commit_detail.as_mut()?;
        match pane {
            DetailPane::Message => {
                let pane_rect = self.msg_area;
                detail.ensure_msg_rows(pane_rect);
                detail.msg_scroll_layout_for(pane_rect, 0)
            }
            DetailPane::Files => Some(ScrollLayout::for_list(
                detail.files.len(),
                scroll_pane::bordered_inner(self.files_area),
                0,
            )),
            DetailPane::Diff => {
                let pane_rect = self.diff_area;
                if !detail.ensure_diff_rows(pane_rect) {
                    return None;
                }
                detail.diff_scroll_layout_for(pane_rect, 0)
            }
        }
    }

    /// Whether `pos` points at one of this panel's scroll indicators. The app asks
    /// before arming a panel-border drag: the indicators run alongside the panel
    /// seam, and losing the grab to the seam would resize panels instead of
    /// scrolling the pane.
    pub(crate) fn is_on_scroll_indicator(&mut self, pos: ratatui::layout::Position) -> bool {
        self.indicator_at(pos).is_some()
    }

    /// The pane whose scroll indicator sits under `pos`, the offset that point
    /// selects on it, and the layout that computed both — the grab stores the
    /// layout so the drag never re-counts the pane's rows.
    fn indicator_at(
        &mut self,
        pos: ratatui::layout::Position,
    ) -> Option<(DetailPane, u16, ScrollLayout)> {
        DetailPane::ALL.into_iter().find_map(|pane| {
            let layout = self.pane_layout(pane)?;
            let offset = scroll_pane::scrub_offset(layout, pos)?;
            Some((pane, offset, layout))
        })
    }

    /// Move the pane a scroll-indicator grab came from to `offset`. The file list
    /// scrolls by moving its highlight, so a scrub there may schedule a new diff.
    fn scrub_to(&mut self, pane: DetailPane, offset: u16) -> Option<Action> {
        match pane {
            DetailPane::Message => {
                if let Some(detail) = self.commit_detail.as_mut() {
                    detail.msg_scroll = offset;
                }
                None
            }
            DetailPane::Diff => {
                if let Some(detail) = self.commit_detail.as_mut() {
                    detail.diff_scroll = offset;
                    // Grabbing the diff's indicator is interacting with the diff:
                    // hand it the keyboard, like a click inside the pane does.
                    detail.diff_focused = true;
                }
                None
            }
            DetailPane::Files => self.scrub_file_highlight(offset),
        }
    }

    /// A scrub on the file list moves the list's window, which a ratatui list only
    /// expresses through its offset and its highlight: both are set, so the frame
    /// that follows shows the screenful the pointer asked for (setting the
    /// highlight alone would let the list re-scroll to keep it visible).
    fn scrub_file_highlight(&mut self, offset: u16) -> Option<Action> {
        let detail = self.commit_detail.as_mut()?;
        // Grabbing the list's indicator hands it the keyboard, the way a row
        // click does — even when the scrub then has nowhere to land (an empty
        // list, or a filter with no matches).
        detail.diff_focused = false;
        let files = detail.files.len();
        if files == 0 {
            return None;
        }
        let index = Self::nearest_file_row(detail, usize::from(offset).min(files - 1))?;
        let already_there =
            detail.file_state.selected() == Some(index) && detail.file_state.offset() == index;
        // Re-request anyway when the pane has no diff to show: that is the state a
        // failed or still-running load leaves behind, and the scrub is then the
        // user's way to ask again (a row click does the same).
        if already_there && detail.diff_content.is_some() {
            return None;
        }
        detail.file_state.select(Some(index));
        *detail.file_state.offset_mut() = index;
        self.file_selection_changed()
    }

    /// The row a scrub should land on: `index` itself, or the closest row the
    /// active filter still matches. The list draws every row (matches are only
    /// bolded), so snapping keeps the scrollbar usable while filtering instead of
    /// dying on the rows a click would refuse.
    fn nearest_file_row(detail: &CommitDetail, index: usize) -> Option<usize> {
        if detail.file_matches(index) {
            return Some(index);
        }
        let matches = detail.file_filter.matches();
        let after = matches.partition_point(|row| *row < index);
        let before = after.checked_sub(1);
        match (before.and_then(|i| matches.get(i)), matches.get(after)) {
            (Some(before), Some(after)) => Some(if index - *before <= *after - index {
                *before
            } else {
                *after
            }),
            (Some(before), None) => Some(*before),
            (None, Some(after)) => Some(*after),
            (None, None) => None,
        }
    }

    /// Continue a scroll-indicator drag: only the row matters, so a pointer that
    /// wanders sideways keeps scrubbing the pane it grabbed. The layout is the
    /// one the grab stored, so a drag over a long pane costs no re-count.
    fn scrub_drag(&mut self, pos: ratatui::layout::Position) -> Option<Action> {
        let (pane, layout) = *self.scrubbing.as_ref()?;
        let offset = scroll_pane::scrub_row_offset(layout, pos.y)?;
        self.scrub_to(pane, offset)
    }
}

impl CommitDetail {
    /// Populate the diff's row-index cache when stale. The overflow check that
    /// decides the width is viewport-bounded; the O(document) index build runs
    /// only on a cache miss. Returns false when there is no diff.
    pub(super) fn ensure_diff_rows(&mut self, pane: Rect) -> bool {
        let Some(content) = self.diff_content.as_deref() else {
            return false;
        };
        let inner = scroll_pane::bordered_inner(pane);
        let visible = usize::from(inner.height);
        let version = self.diff_content_version;
        let hit = matches!(
            &self.diff_rows,
            Some((v, w, _)) if *v == version && *w == inner.width
        );
        if hit {
            // A height-only resize flips the thumb decision without touching
            // the cache key: re-aim the index at the width the current
            // decision implies.
            let (_, _, rows) = self.diff_rows.as_mut().expect("hit");
            rows.retarget(content, inner.width, visible);
        } else {
            self.diff_rows = Some((
                version,
                inner.width,
                scroll_pane::pane_rows(content, inner.width, usize::from(inner.height)),
            ));
        }
        true
    }

    /// The diff pane's scroll layout at `offset`, from the cache that
    /// [`Self::ensure_diff_rows`] refreshed.
    pub(super) fn diff_scroll_layout_for(&self, pane: Rect, offset: u16) -> Option<ScrollLayout> {
        let inner = scroll_pane::bordered_inner(pane);
        let (_, cached_width, rows) = self.diff_rows.as_ref()?;
        debug_assert_eq!(
            *cached_width, inner.width,
            "ensure_diff_rows must run before this for the current pane"
        );
        Some(scroll_pane::pane_scroll_layout(inner, rows, offset))
    }

    /// Populate the message's row-index cache when the content width changed.
    pub(super) fn ensure_msg_rows(&mut self, pane: Rect) {
        let inner = scroll_pane::bordered_inner(pane);
        let visible = usize::from(inner.height);
        let hit = matches!(&self.msg_rows, Some((w, _)) if *w == inner.width);
        if hit {
            // Height-only resize: re-aim the index at the width the current
            // thumb decision implies (see `ensure_diff_rows`).
            let (_, rows) = self.msg_rows.as_mut().expect("hit");
            rows.retarget(&self.message, inner.width, visible);
        } else {
            self.msg_rows = Some((
                inner.width,
                scroll_pane::pane_rows(&self.message, inner.width, usize::from(inner.height)),
            ));
        }
    }

    /// The message pane's scroll layout at `offset`, from the cache that
    /// [`Self::ensure_msg_rows`] refreshed.
    pub(super) fn msg_scroll_layout_for(&self, pane: Rect, offset: u16) -> Option<ScrollLayout> {
        let inner = scroll_pane::bordered_inner(pane);
        let (_, rows) = self.msg_rows.as_ref()?;
        Some(scroll_pane::pane_scroll_layout(inner, rows, offset))
    }
}

impl Component for GitGraph {
    fn cancel_drag(&mut self) {
        self.dragging_detail_border = None;
        self.scrubbing = None;
    }

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
                        step_diff_scroll(detail, self.diff_area, 1);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        step_diff_scroll(detail, self.diff_area, -1);
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
                    self.close_detail();
                    self.reload_pending();
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
            KeyCode::Esc => {
                self.close_detail();
                Ok(None)
            }
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
                // A fresh press always ends a previous indicator grab.
                self.scrubbing = None;
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
                // A pane's scroll indicator belongs to the pane, not to its rows,
                // and not to the detail border it runs alongside either: the
                // column's end rows sit inside a border's grab zone, where a drag
                // would otherwise resize the split instead of scrolling.
                if let Some((pane, offset, layout)) = self.indicator_at(pos) {
                    self.dragging_detail_border = None;
                    self.scrubbing = Some((pane, layout));
                    return Ok(self.scrub_to(pane, offset));
                }
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
                            self.reload_pending();
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
                if self.dragging_detail_border.is_none() && self.scrubbing.is_some() {
                    let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                    return Ok(self.scrub_drag(pos));
                }
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
                            self.files_dragged = true;
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
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_detail_border = None;
                self.scrubbing = None;
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                let mut file_highlight_moved = false;
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        step_diff_scroll(detail, self.diff_area, -1);
                        return Ok(None);
                    }
                    if detail.msg_area.contains(pos) {
                        step_msg_scroll(detail, detail.msg_area, -1);
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
                        step_diff_scroll(detail, self.diff_area, 1);
                        return Ok(None);
                    }
                    if detail.msg_area.contains(pos) {
                        step_msg_scroll(detail, detail.msg_area, 1);
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
    use super::super::render::{files_auto_cells, msg_auto_cells};
    use super::{detail_border_positions, detail_chunks};
    use crate::components::scroll_pane::{self, ScrollLayout, bordered_inner};
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
    fn auto_cell_helpers_stay_inside_u16() {
        // A 65 533-line message used to overflow `line_count + 2` in debug
        // builds; an axis past 1 638 (message) or 2 184 (files) used to
        // overflow the cap's multiply. All representable, none may panic.
        assert_eq!(msg_auto_cells(u16::MAX, 100), 40);
        assert_eq!(msg_auto_cells(u16::MAX, 30), 12);
        assert_eq!(msg_auto_cells(1, u16::MAX), 3);
        assert_eq!(files_auto_cells(30, u16::MAX), 32);
        assert_eq!(files_auto_cells(0, 0), 3);
    }

    #[test]
    fn files_auto_cells_reserves_a_row_per_file_with_cap() {
        assert_eq!(files_auto_cells(2, 60), 4, "two files, two rows");
        assert_eq!(files_auto_cells(30, 60), 18, "capped at 30% of the axis");
        assert_eq!(files_auto_cells(0, 60), 3, "an empty list keeps one row");
        assert_eq!(
            files_auto_cells(9, 10),
            3,
            "never more than the axis allows"
        );
        // No overflow on absurd input, and never a panic from a min > max clamp.
        assert_eq!(files_auto_cells(usize::MAX, 60), 18);
        assert_eq!(files_auto_cells(5, 0), 3);
    }

    #[test]
    fn pane_scrolling_stops_at_content_end() {
        // Largest offset the drawn pane allows; the same clamp a key or wheel
        // step applies.
        let max_offset =
            |text: &str, pane: Rect| scroll_pane::clamp_text_offset(text, pane, u16::MAX);
        // Fits inside the pane: no scrolling allowed.
        let pane = Rect::new(0, 0, 82, 12); // 80x10 inside the borders
        assert_eq!(max_offset("short", pane), 0);
        // 20 rows overflowing a 10-row pane: the offset stops one screenful
        // before the end, so the last row is the last row shown.
        let text = (0..20)
            .map(|_| "x".repeat(60))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(max_offset(&text, pane), 10);
        // Long lines wrap at the width left over beside the scroll indicator
        // (10 - 1 thumb = 9), and the clamp follows that count.
        let long = ["a".repeat(40), "b".repeat(40), "c".repeat(40)].join("\n");
        let small = Rect::new(0, 0, 12, 8); // 10x6 inside the borders
        assert_eq!(scroll_pane::wrapped_rows(&long, 9), 15);
        assert_eq!(max_offset(&long, small), 9); // 15 rows - 6 visible
        // Degenerate panes: too narrow to spare a column never gets one.
        let narrow = Rect::new(0, 0, 1, 80);
        assert_eq!(max_offset(&text, narrow), 0);
        assert!(
            ScrollLayout::for_text(&text, bordered_inner(narrow), 0)
                .bar
                .is_none()
        );
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
