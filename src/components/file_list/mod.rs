use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::scroll_pane::{self, BarColors, ScrollLayout};
use crate::components::{Component, ListFilter};
use crate::git::status::{FileEntry, FileStatus, SubmoduleHead, SubmoduleState, SubmoduleWarn};
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
    /// Memoized row index for `diff_content` (see `scroll_pane::PaneRows`),
    /// keyed by the content version and the content width — the only inputs
    /// the index has. Building it re-wraps the whole document, which a large
    /// diff cannot pay on every frame, scroll step, or click; the per-frame
    /// overflow check that decides the width is viewport-bounded and stays
    /// outside the cache.
    diff_rows: Option<(u64, u16, scroll_pane::PaneRows)>,
    /// Bumped whenever `diff_content` changes, so the index cache can tell a
    /// stale entry from a current one.
    diff_content_version: u64,
    /// The diff's scroll indicator grab, with the scroll layout as it was at
    /// the grab. Kept for the whole drag so the scrub survives the pointer
    /// leaving the indicator column, and so Drag events cost no re-count of
    /// the diff's wrapped rows. A layout that goes stale mid-drag (a new diff
    /// landing under the drag) self-heals at the next press.
    dragging_scrollbar: Option<ScrollLayout>,
    pub horizontal_layout: bool,
    /// Monotonic counter to discard stale DiffLoaded results.
    diff_generation: u64,
    theme: Arc<Theme>,
    /// Substring filter over file paths (`/` to type; matches are highlighted
    /// and `j`/`k` jump between them).
    filter: ListFilter,
    /// True while filter characters are being typed.
    searching: bool,
    /// Path strings mirroring `files`, kept in sync so the filter can rebuild
    /// without re-allocating every keypress.
    path_strings: Vec<String>,
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
            diff_rows: None,
            diff_content_version: 0,
            dragging_scrollbar: None,
            horizontal_layout: false,
            diff_generation: 0,
            theme,
            filter: ListFilter::default(),
            searching: false,
            path_strings: Vec::new(),
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn set_files(&mut self, files: Vec<FileEntry>, repo_name: &str, repo_id: RepoId) {
        let is_same_repo = self.repo_id.as_ref() == Some(&repo_id);
        let prev_selected = self.state.selected();
        let files_changed = !is_same_repo || self.files != files;

        if !is_same_repo {
            self.searching = false;
            self.filter.clear();
        }

        self.files = files;
        self.path_strings = self
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        self.filter.rebuild(&self.path_strings);
        self.repo_name = repo_name.to_string();
        self.repo_id = Some(repo_id);

        if files_changed {
            self.diff_generation += 1;
            self.diff_content_version = self.diff_content_version.wrapping_add(1);
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
        self.normalize_search_selection();
    }

    pub fn search_active(&self) -> bool {
        self.searching && !self.viewing_diff()
    }

    fn normalize_search_selection(&mut self) {
        self.state
            .select(self.filter.selected_match(self.state.selected()));
    }

    fn rebuild_search(&mut self) {
        self.filter.rebuild(&self.path_strings);
        self.normalize_search_selection();
        self.diff_generation += 1;
    }

    pub fn set_diff(&mut self, content: String) {
        self.diff_content = Some(content);
        self.diff_content_version = self.diff_content_version.wrapping_add(1);
        self.diff_scroll = 0;
    }

    fn select_next(&mut self) {
        if self.filter.is_active() || self.searching {
            self.filtered_step(1);
            return;
        }
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
        if self.filter.is_active() || self.searching {
            self.filtered_step(-1);
            return;
        }
        if self.files.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// Move selection to the next/previous matching file (wrapping), matching
    /// the graph search behavior. No-op when nothing matches.
    fn filtered_step(&mut self, delta: isize) {
        self.state
            .select(self.filter.step(self.state.selected(), delta));
    }

    pub fn viewing_diff(&self) -> bool {
        self.diff_content.is_some()
    }

    pub fn selected_path(&self) -> Option<String> {
        let idx = self.state.selected()?;
        let file = self.files.get(idx)?;
        Some(file.path.to_string_lossy().to_string())
    }

    /// The context-menu action for the selected file row, anchored at the
    /// selected row's right edge. Used by the keyboard context menu (`x` /
    /// Menu); mirrors the flags the right-click handler computes.
    pub fn selected_menu(&self) -> Option<Action> {
        let idx = self.state.selected()?;
        let entry = self.files.get(idx)?;
        let repo_id = self.repo_id.clone()?;
        let area = if self.viewing_diff() {
            self.file_list_area
        } else {
            self.render_area
        };
        let row = area
            .y
            .saturating_add(1)
            .saturating_add(idx.saturating_sub(self.state.offset()) as u16);
        let col = area.x.saturating_add(area.width.saturating_sub(1));
        let conflicted = entry.status == FileStatus::Conflicted;
        Some(Action::ShowFileContextMenu {
            id: repo_id,
            path: entry.path.clone(),
            row,
            col,
            staged: entry.staged,
            // A conflicted row counts as stageable: `git add` marks it resolved
            // and Discard restores it.
            unstaged: entry.unstaged || conflicted,
            is_untracked: entry.status == FileStatus::Untracked,
            is_submodule: entry.is_submodule,
        })
    }
    pub fn diff_generation(&self) -> u64 {
        self.diff_generation
    }

    fn try_show_diff(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        if self.filter.is_active() && self.filter.position_of(idx).is_none() {
            return None;
        }
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
            let mut t = format!(" Changes — {} ", self.repo_name);
            if self.searching || self.filter.is_active() {
                t = format!("{t} /{}▏ ", self.filter.query());
            }
            t
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

    /// Populate the row-index cache when stale (content version or pane width
    /// changed). The O(document) index build runs only on a cache miss — the
    /// thumb decision is part of the cached facts, never re-derived per frame.
    /// Returns false when there is no diff.
    fn ensure_diff_rows(&mut self, pane: Rect) -> bool {
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
            // decision implies (O(1) to detect via the cached full-width
            // count; the re-count runs only when the decision actually
            // flipped).
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
    /// [`Self::ensure_diff_rows`] refreshed. `pane` is the diff pane's bordered
    /// rect — the same rect every consumer measures, so one entry serves them
    /// all.
    fn diff_scroll_layout_for(&self, pane: Rect, offset: u16) -> Option<ScrollLayout> {
        let inner = scroll_pane::bordered_inner(pane);
        let (_, cached_width, rows) = self.diff_rows.as_ref()?;
        debug_assert_eq!(
            *cached_width, inner.width,
            "ensure_diff_rows must run before this for the current pane"
        );
        Some(scroll_pane::pane_scroll_layout(inner, rows, offset))
    }

    /// `offset` clamped to the diff pane's last screenful, so a scroll past the
    /// end parks on the final row instead of scrolling into blank space.
    fn clamp_diff_scroll(&mut self, offset: u16) -> u16 {
        let pane = self.diff_area;
        if !self.ensure_diff_rows(pane) {
            return 0;
        }
        self.diff_scroll_layout_for(pane, offset)
            .map(|layout| layout.gauge.offset())
            .unwrap_or(0)
    }

    /// Whether `pos` points at the diff pane's scroll indicator. The app asks
    /// before arming a panel-border drag: the indicator runs alongside the panel
    /// seam, and losing the grab to the seam would resize panels instead of
    /// scrolling the diff.
    pub(crate) fn is_on_scroll_indicator(&mut self, pos: ratatui::layout::Position) -> bool {
        self.diff_scroll_layout()
            .and_then(|layout| scroll_pane::scrub_offset(layout, pos))
            .is_some()
    }

    /// The diff pane's scroll layout as the last frame drew it, or `None` while
    /// there is no diff. The offset does not enter what a scrub needs.
    fn diff_scroll_layout(&mut self) -> Option<ScrollLayout> {
        let pane = self.diff_area;
        if !self.ensure_diff_rows(pane) {
            return None;
        }
        self.diff_scroll_layout_for(pane, 0)
    }

    fn draw_diff(&mut self, frame: &mut Frame, area: Rect) {
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
        let title = format!(" Diff — {} (Esc/h to close) ", self.repo_name);
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
fn submodule_tag_spans(
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

impl Component for FileList {
    fn cancel_drag(&mut self) {
        self.dragging_scrollbar = None;
    }

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
                    let next = self.diff_scroll.saturating_add(1);
                    self.diff_scroll = self.clamp_diff_scroll(next);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let next = self.diff_scroll.saturating_sub(1);
                    self.diff_scroll = self.clamp_diff_scroll(next);
                }
                _ => {}
            }
            return Ok(None);
        }

        if self.searching {
            match key.code {
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.rebuild_search();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.rebuild_search();
                }
                KeyCode::Esc => {
                    self.searching = false;
                    self.filter.clear();
                    self.rebuild_search();
                }
                KeyCode::Down => self.select_next(),
                KeyCode::Up => self.select_prev(),
                KeyCode::Enter => {
                    // No matches: Enter must not open the previously
                    // selected (non-matching) file's diff.
                    if self.filter.count() == 0 {
                        return Ok(None);
                    }
                    return Ok(self.try_show_diff());
                }
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('/') => {
                self.searching = true;
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
            KeyCode::Enter => Ok(self.try_show_diff()),
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                self.dragging_scrollbar = None;

                // The diff's scroll indicator belongs to the diff pane, not to its
                // rows: clicking it jumps there, and the grab scrubs until release.
                if let Some(layout) = self.diff_scroll_layout()
                    && let Some(offset) = scroll_pane::scrub_offset(layout, pos)
                {
                    self.diff_scroll = offset;
                    self.dragging_scrollbar = Some(layout);
                    return Ok(None);
                }

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
                            if self.filter.is_active() && self.filter.position_of(idx).is_none() {
                                return Ok(None);
                            }
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
            MouseEventKind::Down(MouseButton::Right) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                let click_area = if self.viewing_diff() {
                    self.file_list_area
                } else {
                    self.render_area
                };
                if click_area.contains(pos) {
                    let content_y = click_area.y + 1; // +1 for border
                    if mouse.row >= content_y {
                        let idx = (mouse.row - content_y) as usize + self.state.offset();
                        if self.filter.is_active() && self.filter.position_of(idx).is_none() {
                            return Ok(None);
                        }
                        if let (Some(entry), Some(repo_id)) =
                            (self.files.get(idx), self.repo_id.clone())
                        {
                            self.state.select(Some(idx));
                            let conflicted = entry.status == FileStatus::Conflicted;
                            return Ok(Some(Action::ShowFileContextMenu {
                                id: repo_id,
                                path: entry.path.clone(),
                                row: mouse.row,
                                col: mouse.column,
                                staged: entry.staged,
                                // A conflicted row counts as stageable: `git add`
                                // marks it resolved and Discard restores it.
                                unstaged: entry.unstaged || conflicted,
                                is_untracked: entry.status == FileStatus::Untracked,
                                is_submodule: entry.is_submodule,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(layout) = self.dragging_scrollbar {
                    if let Some(offset) = scroll_pane::scrub_row_offset(layout, mouse.row) {
                        self.diff_scroll = offset;
                    }
                    return Ok(None);
                }
                Ok(None)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_scrollbar = None;
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                if self.viewing_diff() {
                    let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                    if self.diff_area.contains(pos) {
                        let next = self.diff_scroll.saturating_sub(1);
                        self.diff_scroll = self.clamp_diff_scroll(next);
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
                        let next = self.diff_scroll.saturating_add(1);
                        self.diff_scroll = self.clamp_diff_scroll(next);
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
mod tests;
