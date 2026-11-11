use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::ListState,
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::scroll_pane::{self, ScrollLayout};
use crate::components::{Component, ListFilter};
use crate::git::stash::StashSource;
use crate::git::status::{FileEntry, FileStatus};
pub(crate) use crate::git::status::{SubmoduleHead, SubmoduleState, SubmoduleWarn};
use crate::repo_id::RepoId;
pub(crate) use crate::theme::FileListTheme;
use crate::theme::Theme;

mod render;
mod source;

pub(crate) use source::{ContentState, FileSource, StashFileEntry, StashView};

pub(crate) struct FileList {
    files: Vec<FileEntry>,
    state: ListState,
    /// Which repository/snapshot the rows came from. `None` before the first
    /// selection.
    source: Option<FileSource>,
    /// Parallel metadata for `files` when `source` is a stash. Kept separate
    /// from `FileEntry` because stash rows carry a read-only origin and both
    /// sides of a rename.
    stash_files: Option<Vec<StashFileEntry>>,
    content_state: ContentState,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    render_area: Rect,
    file_list_area: Rect,
    diff_area: Rect,
    // Diff view
    diff_content: Option<String>,
    diff_path: Option<std::path::PathBuf>,
    /// Origin of `diff_path` inside a stash. The same path can be present as
    /// both tracked and saved-untracked content, so the path alone does not
    /// identify the row whose diff is open.
    diff_stash_source: Option<StashSource>,
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
    /// Monotonic counter for stash file-list requests. Because A → B → A
    /// selects the same snapshot twice, the snapshot identity cannot tell the
    /// two visits apart; the request that is still outstanding can.
    stash_generation: u64,
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
            source: None,
            stash_files: None,
            content_state: ContentState::Ready,
            focused: false,
            action_tx: None,
            render_area: Rect::default(),
            file_list_area: Rect::default(),
            diff_area: Rect::default(),
            diff_content: None,
            diff_path: None,
            diff_stash_source: None,
            diff_scroll: 0,
            diff_rows: None,
            diff_content_version: 0,
            dragging_scrollbar: None,
            horizontal_layout: false,
            diff_generation: 0,
            stash_generation: 0,
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
        let source = FileSource::WorkingTree {
            repo_id,
            name: repo_name.to_string(),
        };
        let is_same_source = self.source.as_ref() == Some(&source);
        let prev_selected = self.state.selected();
        let selected_path = prev_selected
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.clone());
        let files_changed = !is_same_source || self.files != files;

        if !is_same_source {
            self.searching = false;
            self.filter.clear();
        }

        self.files = files;
        self.stash_files = None;
        self.diff_stash_source = None;
        self.content_state = ContentState::Ready;
        self.source = Some(source);
        self.rebuild_path_strings();

        let keep_diff = is_same_source
            && self
                .diff_path
                .as_ref()
                .is_some_and(|path| self.files.iter().any(|f| f.path == *path));
        if files_changed && !keep_diff {
            self.reset_diff();
        }

        if self.files.is_empty() {
            self.state.select(None);
        } else if is_same_source {
            // Preserve selection on refresh
            let idx = selected_path
                .as_ref()
                .and_then(|path| self.files.iter().position(|f| f.path == *path))
                .or(prev_selected)
                .map(|i| i.min(self.files.len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        } else {
            self.state.select(Some(0));
        }
        self.normalize_search_selection();
    }

    /// Enter the read-only preview for `oid` before its rows have loaded.
    /// Returns the generation of this list request; a result tagged with an
    /// older generation is stale even if it names the same snapshot.
    pub fn show_stash_loading(
        &mut self,
        repo_id: RepoId,
        repo_name: String,
        oid: String,
        label: String,
        summary: String,
    ) -> u64 {
        self.stash_generation += 1;
        self.source = Some(FileSource::Stash(StashView {
            repo_id,
            repo_name,
            oid,
            label,
            summary,
        }));
        self.stash_files = None;
        self.files.clear();
        self.path_strings.clear();
        self.filter.clear();
        self.searching = false;
        self.state.select(None);
        self.content_state = ContentState::Loading;
        self.reset_diff();
        self.stash_generation
    }

    /// Install the rows of the selected stash. `path_strings` keeps rename
    /// rows readable (`old → new`) while `files` keeps the diff key in `path`.
    pub fn set_stash_files(&mut self, stash_files: Vec<StashFileEntry>) {
        let is_same_source = self.is_stash();
        let prev_selected = self.state.selected();
        // A stash can hold the same path as both tracked and saved-untracked
        // content, so the selected row is remembered by its full identity.
        let selected_identity = prev_selected
            .and_then(|i| self.stash_files.as_ref()?.get(i))
            .map(|f| (f.source, f.path.clone()));

        self.files = stash_files
            .iter()
            .map(StashFileEntry::as_file_entry)
            .collect();
        self.path_strings = stash_files
            .iter()
            .map(StashFileEntry::display_path)
            .collect();
        self.stash_files = Some(stash_files);
        self.content_state = ContentState::Ready;
        self.filter.rebuild(&self.path_strings);

        // The open diff survives a reload only if the new list still holds the
        // same (source, path) row.
        let keep_diff = match (&self.diff_path, self.diff_stash_source) {
            (Some(path), Some(source)) => self
                .stash_files
                .as_ref()
                .is_some_and(|files| files.iter().any(|f| f.source == source && f.path == *path)),
            _ => false,
        };
        if !is_same_source || !keep_diff {
            self.reset_diff();
        }

        if self.files.is_empty() {
            self.state.select(None);
        } else if is_same_source {
            let idx = selected_identity
                .as_ref()
                .and_then(|(source, path)| {
                    self.stash_files
                        .as_ref()?
                        .iter()
                        .position(|f| f.source == *source && f.path == *path)
                })
                .or(prev_selected)
                .map(|i| i.min(self.files.len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        } else {
            self.state.select(Some(0));
        }
        self.normalize_search_selection();
    }

    pub fn set_stash_error(&mut self, message: String) {
        self.stash_files = None;
        self.files.clear();
        self.path_strings.clear();
        self.filter.clear();
        self.searching = false;
        self.state.select(None);
        self.content_state = ContentState::Error(message);
        self.reset_diff();
    }

    fn rebuild_path_strings(&mut self) {
        self.path_strings = self
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        self.filter.rebuild(&self.path_strings);
    }

    fn reset_diff(&mut self) {
        self.diff_generation += 1;
        self.diff_content_version = self.diff_content_version.wrapping_add(1);
        self.diff_content = None;
        self.diff_path = None;
        self.diff_stash_source = None;
        self.diff_scroll = 0;
    }

    pub fn is_stash(&self) -> bool {
        matches!(self.source, Some(FileSource::Stash(_)))
    }

    pub fn stash_view(&self) -> Option<&StashView> {
        match self.source.as_ref()? {
            FileSource::Stash(stash) => Some(stash),
            FileSource::WorkingTree { .. } => None,
        }
    }

    /// Update the display identity of the current stash preview when the
    /// stash list renumbers it. Content and selection stay bound to the OID.
    pub fn update_stash_identity(
        &mut self,
        repo_id: &RepoId,
        oid: &str,
        label: String,
        summary: String,
    ) {
        let Some(FileSource::Stash(stash)) = self.source.as_mut() else {
            return;
        };
        if &stash.repo_id == repo_id && stash.oid == oid {
            stash.label = label;
            stash.summary = summary;
        }
    }

    /// Whether the panel is still showing exactly the requested snapshot.
    pub fn is_viewing_stash(&self, repo_id: &RepoId, oid: &str) -> bool {
        self.stash_view()
            .is_some_and(|stash| &stash.repo_id == repo_id && stash.oid == oid)
    }

    /// Whether a stash file-list result belongs to the snapshot that is still
    /// displayed and to the latest list request. Both checks are needed:
    /// A → B → A selects the same snapshot twice, so identity alone would let
    /// the first visit's late result overwrite the second visit's preview.
    pub fn accepts_stash_files(&self, repo_id: &RepoId, oid: &str, generation: u64) -> bool {
        generation == self.stash_generation && self.is_viewing_stash(repo_id, oid)
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
        if self.diff_content.is_none() {
            self.diff_scroll = 0;
        }
        self.diff_content = Some(content);
        self.diff_content_version = self.diff_content_version.wrapping_add(1);
    }

    pub fn refresh_diff(&mut self) -> Option<Action> {
        let path = self.diff_path.clone()?;
        if let Some(stash) = self.stash_view() {
            let source = self.diff_stash_source?;
            let file = self
                .stash_files
                .as_ref()?
                .iter()
                .find(|file| file.source == source && file.path == path)?
                .clone();
            let repo_id = stash.repo_id.clone();
            let oid = stash.oid.clone();
            self.diff_generation += 1;
            return Some(Action::ShowStashDiff {
                repo_id,
                oid,
                source: file.source,
                path: file.path,
                generation: self.diff_generation,
            });
        }
        let repo_id = self.source.as_ref()?.repo_id().clone();
        self.diff_generation += 1;
        Some(Action::ShowDiff(repo_id, path))
    }

    pub fn start_diff(&mut self, repo_id: &RepoId, path: &std::path::Path) -> Option<u64> {
        if self.source.as_ref().map(FileSource::repo_id) != Some(repo_id)
            || self.is_stash()
            || self.diff_path.as_deref() != Some(path)
        {
            return None;
        }
        self.diff_generation += 1;
        Some(self.diff_generation)
    }

    /// Claim the generation for a stash diff request. The caller must have
    /// bumped it when emitting [`Action::ShowStashDiff`].
    pub fn start_stash_diff(
        &self,
        repo_id: &RepoId,
        oid: &str,
        path: &std::path::Path,
        generation: u64,
    ) -> Option<u64> {
        if generation != self.diff_generation
            || !self.is_viewing_stash(repo_id, oid)
            || self.diff_path.as_deref() != Some(path)
        {
            return None;
        }
        Some(generation)
    }

    /// A late stash diff may only land on the snapshot and path that asked
    /// for it, and only if no newer diff request superseded it.
    pub fn accepts_stash_diff(
        &self,
        repo_id: &RepoId,
        oid: &str,
        path: &std::path::Path,
        generation: u64,
    ) -> bool {
        generation == self.diff_generation
            && self.is_viewing_stash(repo_id, oid)
            && self.diff_path.as_deref() == Some(path)
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
        if self.is_stash() {
            return None;
        }
        let idx = self.state.selected()?;
        let entry = self.files.get(idx)?;
        let repo_id = self.source.as_ref()?.repo_id().clone();
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
        if let Some(stash) = self.stash_view() {
            let file = self.stash_files.as_ref()?.get(idx)?.clone();
            let repo_id = stash.repo_id.clone();
            let oid = stash.oid.clone();
            self.diff_generation += 1;
            self.diff_path = Some(file.path.clone());
            self.diff_stash_source = Some(file.source);
            self.diff_scroll = 0;
            return Some(Action::ShowStashDiff {
                repo_id,
                oid,
                source: file.source,
                path: file.path,
                generation: self.diff_generation,
            });
        }
        let repo_id = self.source.as_ref()?.repo_id().clone();
        let file = self.files.get(idx)?;
        self.diff_generation += 1;
        self.diff_path = Some(file.path.clone());
        self.diff_scroll = 0;
        Some(Action::ShowDiff(repo_id, file.path.clone()))
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

    /// The rendered text pane under the pointer, excluding borders and its bar.
    pub(crate) fn text_selection_area(&mut self, pos: ratatui::layout::Position) -> Option<Rect> {
        let pane = [self.file_list_area, self.diff_area]
            .into_iter()
            .find(|area| area.contains(pos))?;
        let content = if pane == self.diff_area {
            self.diff_scroll_layout()?.content
        } else {
            scroll_pane::bordered_inner(pane)
        };
        content.contains(pos).then_some(content)
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
                    self.diff_path = None;
                    self.diff_stash_source = None;
                    self.diff_generation += 1;
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
            KeyCode::Esc if self.is_stash() => Ok(Some(Action::ExitStashView)),
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
                if self.is_stash() {
                    // Stash rows have no workspace context menu. The dispatch
                    // boundary also refuses those actions if one slips through.
                    return Ok(None);
                }
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
                        if let (Some(entry), Some(repo_id)) = (
                            self.files.get(idx),
                            self.source.as_ref().map(FileSource::repo_id).cloned(),
                        ) {
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
