use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::status::RepoStatus;
use crate::repo_id::RepoId;
use crate::theme::Theme;

/// Result of `RepoList::sync_paths` — the paths that newly appeared and the
/// paths that vanished. Empty `added` and `removed` mean the set is unchanged
/// and no rebuild ran.
#[derive(Default, Clone, Debug)]
pub(crate) struct SyncDiff {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Half-open column ranges `[start, end)` of the subtree indicators rendered
/// on a repo row. `None` means the indicator is not drawn for this entry.
/// Used by the click handler to route a click to the right toggle.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct IndicatorColumns {
    stash: Option<(u16, u16)>,
    worktree: Option<(u16, u16)>,
}

/// Compute the column ranges of the stash and worktree indicators for a repo
/// row, given the leftmost content column. Mirrors the layout in
/// [`RepoList::render_repo_item`] — keep the two in sync.
fn indicator_columns(entry: &RepoEntry, base_x: u16) -> IndicatorColumns {
    let mut col = base_x;
    // Dirty/git_op marker: always 2 columns ("* ", "~ ", or "  ").
    col = col.saturating_add(2);

    let mut out = IndicatorColumns::default();
    let Some(status) = entry.status.as_ref() else {
        return out;
    };

    // Branch field is left-padded to a minimum of 12 columns, then a space.
    let branch_width = status.branch.chars().count().max(12).saturating_add(1);
    col = col.saturating_add(branch_width as u16);

    if status.ahead > 0 {
        // "↑{N} " — chevron (1) + digits + trailing space.
        col = col.saturating_add(2 + status.ahead.to_string().len() as u16);
    }
    if status.behind > 0 {
        col = col.saturating_add(2 + status.behind.to_string().len() as u16);
    }

    if !status.stashes.is_empty() {
        let start = col;
        // "▶$N " — chevron (1) + '$' (1) + digits + trailing space.
        let width = 3 + status.stash_count().to_string().len() as u16;
        out.stash = Some((start, start.saturating_add(width)));
        col = col.saturating_add(width);
    }

    if !status.worktree_info.is_empty() {
        let start = col;
        // "▶N " — chevron (1) + digits + trailing space.
        let width = 2 + status.worktree_info.len().to_string().len() as u16;
        out.worktree = Some((start, start.saturating_add(width)));
    }

    out
}

impl SyncDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RepoEntry {
    pub path: PathBuf,
    pub name: String,
    pub status: Option<RepoStatus>,
    /// True only during push/pull/rebase — shows animated spinner
    pub git_op: bool,
}

/// Maps a visual row in the list to either a repo, one of its worktrees,
/// or one of its stash entries.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DisplayRow {
    Repo(usize),
    Worktree(usize, usize), // (repo_index, worktree_index)
    Stash(usize, usize),    // (repo_index, stash_index_in_status.stashes)
}

pub(crate) struct RepoList {
    pub repos: Vec<RepoEntry>,
    pub state: ListState,
    pub render_area: Rect,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    /// Which repos have their worktree list expanded
    expanded_repos: HashSet<RepoId>,
    /// Which repos have their stash list expanded
    expanded_stashes: HashSet<RepoId>,
    /// Computed mapping from visual row → data
    display_rows: Vec<DisplayRow>,
    theme: Arc<Theme>,
}

impl RepoList {
    pub fn new(repo_paths: Vec<PathBuf>, theme: Arc<Theme>) -> Self {
        let repos: Vec<RepoEntry> = repo_paths
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                RepoEntry {
                    path,
                    name,
                    status: None,
                    git_op: false,
                }
            })
            .collect();

        let mut state = ListState::default();
        if !repos.is_empty() {
            state.select(Some(0));
        }

        let mut list = Self {
            repos,
            state,
            render_area: Rect::default(),
            focused: true,
            action_tx: None,
            expanded_repos: HashSet::new(),
            expanded_stashes: HashSet::new(),
            display_rows: Vec::new(),
            theme,
        };
        list.rebuild_display_rows();
        list
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    /// Recompute display_rows from repos + expansion state.
    fn rebuild_display_rows(&mut self) {
        self.display_rows.clear();
        for (i, entry) in self.repos.iter().enumerate() {
            self.display_rows.push(DisplayRow::Repo(i));
            let id = RepoId(entry.path.clone());
            if let Some(status) = &entry.status {
                if self.expanded_repos.contains(&id) {
                    for j in 0..status.worktree_info.len() {
                        self.display_rows.push(DisplayRow::Worktree(i, j));
                    }
                }
                if self.expanded_stashes.contains(&id) {
                    for j in 0..status.stashes.len() {
                        self.display_rows.push(DisplayRow::Stash(i, j));
                    }
                }
            }
        }
    }

    /// Returns the parent repo index for the current selection.
    pub fn selected_index(&self) -> Option<usize> {
        let di = self.state.selected()?;
        match self.display_rows.get(di)? {
            DisplayRow::Repo(i) => Some(*i),
            DisplayRow::Worktree(ri, _) => Some(*ri),
            DisplayRow::Stash(ri, _) => Some(*ri),
        }
    }

    /// Resolve a stable `RepoId` to its current positional index.
    pub fn resolve_index(&self, id: &RepoId) -> Option<usize> {
        self.repos.iter().position(|e| e.path == id.0)
    }

    /// Returns the parent RepoEntry for the current selection.
    pub fn selected_repo(&self) -> Option<&RepoEntry> {
        self.selected_index().and_then(|i| self.repos.get(i))
    }

    /// If a worktree row is currently selected, returns the parent repo path
    /// and the worktree details. Returns None when a repo row is selected.
    #[allow(dead_code)]
    pub fn selected_worktree(&self) -> Option<(RepoId, &crate::git::status::WorktreeEntry)> {
        let di = self.state.selected()?;
        match self.display_rows.get(di)? {
            DisplayRow::Repo(_) | DisplayRow::Stash(_, _) => None,
            DisplayRow::Worktree(ri, wi) => {
                let entry = self.repos.get(*ri)?;
                let wt = entry.status.as_ref()?.worktree_info.get(*wi)?;
                Some((RepoId(entry.path.clone()), wt))
            }
        }
    }

    /// Select the display row corresponding to a repo index.
    /// Used by app.rs when it needs to programmatically select a repo.
    pub fn select_repo_row(&mut self, repo_idx: usize) {
        for (di, row) in self.display_rows.iter().enumerate() {
            if matches!(row, DisplayRow::Repo(i) if *i == repo_idx) {
                self.state.select(Some(di));
                return;
            }
        }
    }

    fn select_next(&mut self) {
        if self.display_rows.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.display_rows.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.display_rows.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// In-place merge of a freshly discovered repo path list.
    ///
    /// Existing entries are kept (preserves their `status` and `git_op` flags);
    /// vanished entries are dropped along with their expansion state. Returns
    /// the paths that were added and removed so the caller can prune related
    /// per-repo state (pending status queries, dirty markers, active worktree).
    /// Returns an empty diff (and skips the rebuild) when the set is unchanged.
    pub fn sync_paths(&mut self, new_paths: Vec<PathBuf>) -> SyncDiff {
        let current: HashSet<PathBuf> = self.repos.iter().map(|r| r.path.clone()).collect();
        let desired: HashSet<PathBuf> = new_paths.iter().cloned().collect();

        if current == desired
            && new_paths
                .iter()
                .zip(self.repos.iter())
                .all(|(p, e)| p == &e.path)
        {
            return SyncDiff::default();
        }

        let mut by_path: std::collections::HashMap<PathBuf, RepoEntry> =
            self.repos.drain(..).map(|e| (e.path.clone(), e)).collect();

        let mut next: Vec<RepoEntry> = Vec::with_capacity(new_paths.len());
        let mut added: Vec<PathBuf> = Vec::new();
        for path in &new_paths {
            if let Some(existing) = by_path.remove(path) {
                next.push(existing);
            } else {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                next.push(RepoEntry {
                    path: path.clone(),
                    name,
                    status: None,
                    git_op: false,
                });
                added.push(path.clone());
            }
        }

        let removed: Vec<PathBuf> = by_path.into_keys().collect();
        for path in &removed {
            let id = RepoId(path.clone());
            self.expanded_repos.remove(&id);
            self.expanded_stashes.remove(&id);
        }

        self.repos = next;
        self.rebuild_display_rows();

        SyncDiff { added, removed }
    }

    pub fn update_status(&mut self, index: usize, repo_status: RepoStatus) {
        if let Some(entry) = self.repos.get_mut(index) {
            entry.status = Some(repo_status);
            entry.git_op = false;
        }
        self.rebuild_display_rows();
    }

    /// Build the action to emit for the current selection.
    fn emit_selection_action(&self) -> Option<Action> {
        let di = self.state.selected()?;
        match self.display_rows.get(di)? {
            DisplayRow::Repo(i) => {
                let id = RepoId(self.repos[*i].path.clone());
                Some(Action::SelectRepo(id))
            }
            DisplayRow::Worktree(ri, wi) => {
                let entry = &self.repos[*ri];
                let wt = entry.status.as_ref()?.worktree_info.get(*wi)?;
                Some(Action::SelectWorktree {
                    repo_id: RepoId(entry.path.clone()),
                    worktree_path: wt.path.clone(),
                    worktree_branch: wt.branch.clone(),
                })
            }
            DisplayRow::Stash(ri, _) => {
                // Re-target the file list / graph to the stash's parent repo
                // (so a stash row click from another repo updates the right
                // details), but do NOT call select_repo_row — that would
                // snap the cursor off the stash and back onto the Repo row.
                let id = RepoId(self.repos[*ri].path.clone());
                Some(Action::FocusRepoDetails(id))
            }
        }
    }

    /// Selection's parent repo index, ignoring stash/worktree depth.
    fn current_parent_repo(&self) -> Option<usize> {
        let di = self.state.selected()?;
        match self.display_rows.get(di)? {
            DisplayRow::Repo(i) => Some(*i),
            DisplayRow::Worktree(ri, _) => Some(*ri),
            DisplayRow::Stash(ri, _) => Some(*ri),
        }
    }

    /// Snapshot the currently-selected DisplayRow before a rebuild that may
    /// reshuffle indices (subtree expand/collapse).
    fn snapshot_selection(&self) -> Option<DisplayRow> {
        let di = self.state.selected()?;
        self.display_rows.get(di).cloned()
    }

    /// Re-select the same logical row after rebuild_display_rows runs.
    /// Falls back to the parent Repo row if the previous row is gone (e.g.
    /// its subtree was collapsed).
    fn restore_selection(&mut self, prev: Option<DisplayRow>) {
        let Some(prev) = prev else { return };
        let parent_idx = match &prev {
            DisplayRow::Repo(i) => *i,
            DisplayRow::Worktree(ri, _) => *ri,
            DisplayRow::Stash(ri, _) => *ri,
        };
        if let Some(new_idx) = self.display_rows.iter().position(|r| *r == prev) {
            self.state.select(Some(new_idx));
        } else {
            self.select_repo_row(parent_idx);
        }
    }

    /// Toggle stash expansion for the repo at the current selection.
    fn toggle_stash_expand(&mut self) {
        let Some(repo_idx) = self.current_parent_repo() else {
            return;
        };
        let entry = &self.repos[repo_idx];
        let has_stashes = entry.status.as_ref().is_some_and(|s| !s.stashes.is_empty());
        if !has_stashes {
            return;
        }
        let id = RepoId(entry.path.clone());
        let prev = self.snapshot_selection();
        if self.expanded_stashes.contains(&id) {
            self.expanded_stashes.remove(&id);
            self.rebuild_display_rows();
            self.select_repo_row(repo_idx);
        } else {
            self.expanded_stashes.insert(id);
            self.rebuild_display_rows();
            self.restore_selection(prev);
        }
    }

    /// Toggle worktree expansion for the repo at the current selection.
    fn toggle_expand(&mut self) {
        let Some(di) = self.state.selected() else {
            return;
        };
        let repo_idx = match self.display_rows.get(di) {
            Some(DisplayRow::Repo(i)) => *i,
            Some(DisplayRow::Worktree(ri, _)) => *ri,
            Some(DisplayRow::Stash(ri, _)) => *ri,
            None => return,
        };
        let entry = &self.repos[repo_idx];
        let has_worktrees = entry
            .status
            .as_ref()
            .is_some_and(|s| !s.worktree_info.is_empty());
        if !has_worktrees {
            return;
        }
        let id = RepoId(entry.path.clone());
        let prev = self.snapshot_selection();
        if self.expanded_repos.contains(&id) {
            // Collapsing: move selection to the parent repo row
            self.expanded_repos.remove(&id);
            self.rebuild_display_rows();
            self.select_repo_row(repo_idx);
        } else {
            self.expanded_repos.insert(id);
            self.rebuild_display_rows();
            self.restore_selection(prev);
        }
    }

    fn render_repo_item(&self, entry: &RepoEntry, _repo_idx: usize) -> ListItem<'static> {
        let t = &self.theme.repo_list;
        let mut spans = Vec::new();

        if entry.git_op {
            spans.push(Span::styled("~ ", Style::default().fg(t.git_op_marker)));
        } else if entry.status.as_ref().map(|s| s.is_dirty).unwrap_or(false) {
            spans.push(Span::styled("* ", Style::default().fg(t.dirty_marker)));
        } else {
            spans.push(Span::raw("  "));
        }

        if let Some(status) = &entry.status {
            spans.push(Span::styled(
                format!("{:<12} ", status.branch),
                Style::default().fg(t.branch),
            ));

            if status.ahead > 0 {
                spans.push(Span::styled(
                    format!("\u{2191}{} ", status.ahead),
                    Style::default().fg(t.ahead),
                ));
            }
            if status.behind > 0 {
                spans.push(Span::styled(
                    format!("\u{2193}{} ", status.behind),
                    Style::default().fg(t.behind),
                ));
            }

            if !status.stashes.is_empty() {
                let id = RepoId(entry.path.clone());
                let expanded = self.expanded_stashes.contains(&id);
                let icon = if expanded { "\u{25bc}" } else { "\u{25b6}" };
                spans.push(Span::styled(
                    format!("{}${} ", icon, status.stash_count()),
                    Style::default().fg(t.stash),
                ));
            }

            if !status.worktree_info.is_empty() {
                let id = RepoId(entry.path.clone());
                let expanded = self.expanded_repos.contains(&id);
                let icon = if expanded { "\u{25bc}" } else { "\u{25b6}" };
                spans.push(Span::styled(
                    format!("{}{} ", icon, status.worktree_info.len()),
                    Style::default().fg(t.worktree_count),
                ));
            }

            if status.has_dirty_submodules {
                spans.push(Span::styled(
                    "\u{25c8} ",
                    Style::default().fg(t.dirty_submodule),
                ));
            }

            if status.has_unpushed_submodules {
                spans.push(Span::styled(
                    "\u{21e1} ",
                    Style::default().fg(t.unpushed_submodule),
                ));
            }

            if status.fetch_failed {
                spans.push(Span::styled(
                    "\u{26a0} ",
                    Style::default().fg(t.fetch_failed),
                ));
            }

            if !status.files.is_empty() {
                spans.push(Span::styled(
                    format!("[{}] ", status.files.len()),
                    Style::default().fg(t.file_count),
                ));
            }
        }

        spans.push(Span::styled(
            entry.name.clone(),
            Style::default().fg(t.repo_name),
        ));

        ListItem::new(Line::from(spans))
    }

    fn render_worktree_item(&self, entry: &RepoEntry, wt_idx: usize) -> ListItem<'static> {
        let t = &self.theme.repo_list;
        let wt = &entry.status.as_ref().unwrap().worktree_info[wt_idx];
        let spans = vec![
            Span::styled(
                "    \u{2387} ",
                Style::default().fg(t.worktree_subtree_icon),
            ),
            Span::styled(
                wt.branch.clone(),
                Style::default().fg(t.worktree_subtree_branch),
            ),
        ];
        ListItem::new(Line::from(spans))
    }

    fn render_stash_item(&self, entry: &RepoEntry, stash_idx: usize) -> ListItem<'static> {
        let t = &self.theme.repo_list;
        let stash = &entry.status.as_ref().unwrap().stashes[stash_idx];
        let label = format!("    $ stash@{{{}}} ", stash.index);
        let spans = vec![
            Span::styled(label, Style::default().fg(t.stash)),
            Span::styled(
                stash.message.clone(),
                Style::default().fg(t.worktree_subtree_icon),
            ),
        ];
        ListItem::new(Line::from(spans))
    }
}

impl Component for RepoList {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('w') => {
                self.toggle_expand();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('S') => {
                self.toggle_stash_expand();
                Ok(self.emit_selection_action())
            }
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.display_rows.len() {
                        // Click on the already-selected repo row toggles a subtree.
                        // When both worktrees and stashes are present we hit-test
                        // the click column against each indicator's range so the
                        // user can target either. A click elsewhere on the row
                        // falls back to whichever subtree exists (worktrees first).
                        if self.state.selected() == Some(idx)
                            && let Some(DisplayRow::Repo(i)) = self.display_rows.get(idx)
                            && let Some(status) = self.repos[*i].status.as_ref()
                        {
                            let base_x = self.render_area.x + 1;
                            let cols = indicator_columns(&self.repos[*i], base_x);
                            let clicked_stash = cols
                                .stash
                                .is_some_and(|(s, e)| mouse.column >= s && mouse.column < e);
                            let clicked_worktree = cols
                                .worktree
                                .is_some_and(|(s, e)| mouse.column >= s && mouse.column < e);
                            if clicked_stash {
                                self.toggle_stash_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if clicked_worktree {
                                self.toggle_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if !status.worktree_info.is_empty() {
                                self.toggle_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if !status.stashes.is_empty() {
                                self.toggle_stash_expand();
                                return Ok(self.emit_selection_action());
                            }
                        }
                        self.state.select(Some(idx));
                        return Ok(self.emit_selection_action());
                    }
                }
                Ok(None)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.display_rows.len() {
                        self.state.select(Some(idx));
                        // Only show context menu for repo rows
                        if let Some(DisplayRow::Repo(i)) = self.display_rows.get(idx) {
                            let id = RepoId(self.repos[*i].path.clone());
                            return Ok(Some(Action::ShowContextMenu {
                                id,
                                row: mouse.row,
                                col: mouse.column,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            _ => Ok(None),
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::SelectNextRepo => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            Action::SelectPrevRepo => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            Action::RepoStatusUpdated { ref id, ref status } => {
                if let Some(idx) = self.resolve_index(id) {
                    self.update_status(idx, status.clone());
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;
        // Ensure display_rows is fresh
        self.rebuild_display_rows();

        let items: Vec<ListItem> = self
            .display_rows
            .iter()
            .map(|row| match row {
                DisplayRow::Repo(i) => self.render_repo_item(&self.repos[*i], *i),
                DisplayRow::Worktree(ri, wi) => self.render_worktree_item(&self.repos[*ri], *wi),
                DisplayRow::Stash(ri, si) => self.render_stash_item(&self.repos[*ri], *si),
            })
            .collect();

        let t = &self.theme.repo_list;
        let border_color = if self.focused {
            t.border_focused
        } else {
            t.border_unfocused
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Repositories ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, area, &mut self.state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::{RepoStatus, StashEntry, WorktreeEntry};

    fn empty_status(branch: &str) -> RepoStatus {
        RepoStatus {
            branch: branch.to_string(),
            files: Vec::new(),
            ahead: 0,
            behind: 0,
            is_dirty: false,
            worktree_info: Vec::new(),
            has_submodules: false,
            submodules: Vec::new(),
            has_dirty_submodules: false,
            has_unpushed_submodules: false,
            fetch_failed: false,
            stashes: Vec::new(),
        }
    }

    fn stash_entry(index: usize) -> StashEntry {
        StashEntry {
            index,
            message: format!("WIP {index}"),
            oid: format!("{index:040x}"),
        }
    }

    fn worktree_entry(name: &str) -> WorktreeEntry {
        WorktreeEntry {
            name: name.to_string(),
            path: PathBuf::from(format!("/wt/{name}")),
            branch: name.to_string(),
        }
    }

    fn entry_with_status(path: &str, status: RepoStatus) -> RepoEntry {
        RepoEntry {
            path: PathBuf::from(path),
            name: path.trim_start_matches('/').to_string(),
            status: Some(status),
            git_op: false,
        }
    }

    fn make_list(paths: &[&str]) -> RepoList {
        let theme = Arc::new(Theme::default());
        RepoList::new(paths.iter().map(PathBuf::from).collect(), theme)
    }

    #[test]
    fn sync_paths_noop_when_set_unchanged() {
        let mut list = make_list(&["/a", "/b"]);
        let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(diff.is_empty());
        assert_eq!(list.repos.len(), 2);
    }

    #[test]
    fn sync_paths_reports_added_paths() {
        let mut list = make_list(&["/a"]);
        let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(!diff.is_empty());
        assert_eq!(diff.added, vec![PathBuf::from("/b")]);
        assert!(diff.removed.is_empty());
        assert_eq!(list.repos.len(), 2);
    }

    #[test]
    fn sync_paths_reports_removed_paths_and_prunes_expansion() {
        let mut list = make_list(&["/a", "/b"]);
        list.expanded_repos.insert(RepoId(PathBuf::from("/b")));
        list.expanded_stashes.insert(RepoId(PathBuf::from("/b")));

        let diff = list.sync_paths(vec![PathBuf::from("/a")]);
        assert_eq!(diff.removed, vec![PathBuf::from("/b")]);
        assert!(diff.added.is_empty());
        assert_eq!(list.repos.len(), 1);
        assert!(!list.expanded_repos.contains(&RepoId(PathBuf::from("/b"))));
        assert!(!list.expanded_stashes.contains(&RepoId(PathBuf::from("/b"))));
    }

    #[test]
    fn sync_paths_preserves_existing_entry_status() {
        let mut list = make_list(&["/a"]);
        list.repos[0].git_op = true;
        let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(diff.added, vec![PathBuf::from("/b")]);
        // Original entry kept its in-progress flag.
        let a = list
            .repos
            .iter()
            .find(|r| r.path == std::path::Path::new("/a"))
            .unwrap();
        assert!(a.git_op);
        // Newly-added entry starts clean.
        let b = list
            .repos
            .iter()
            .find(|r| r.path == std::path::Path::new("/b"))
            .unwrap();
        assert!(!b.git_op);
        assert!(b.status.is_none());
    }

    #[test]
    fn indicator_columns_returns_none_when_neither_subtree_present() {
        let entry = entry_with_status("/r", empty_status("main"));
        let cols = indicator_columns(&entry, 0);
        assert_eq!(cols.stash, None);
        assert_eq!(cols.worktree, None);
    }

    #[test]
    fn indicator_columns_locates_stash_then_worktree() {
        let mut status = empty_status("main");
        status.stashes.push(stash_entry(0));
        status.worktree_info.push(worktree_entry("wt"));
        let entry = entry_with_status("/r", status);

        let cols = indicator_columns(&entry, 0);
        // 2 (dirty marker) + 12 (branch pad for "main") + 1 (space) = 15 → stash start.
        let (s_start, s_end) = cols.stash.expect("stash range");
        let (w_start, w_end) = cols.worktree.expect("worktree range");
        assert_eq!(s_start, 15);
        // "▶$1 " = 4 columns.
        assert_eq!(s_end, 19);
        // Worktree starts immediately after stash.
        assert_eq!(w_start, 19);
        // "▶1 " = 3 columns.
        assert_eq!(w_end, 22);
    }

    #[test]
    fn indicator_columns_accounts_for_ahead_behind() {
        let mut status = empty_status("main");
        status.ahead = 3;
        status.behind = 22;
        status.stashes.push(stash_entry(0));
        let entry = entry_with_status("/r", status);

        let cols = indicator_columns(&entry, 0);
        // 2 + 13 (branch) + 3 ("↑3 ") + 4 ("↓22 ") = 22 → stash start.
        let (s_start, _) = cols.stash.expect("stash range");
        assert_eq!(s_start, 22);
    }

    #[test]
    fn indicator_columns_respects_long_branch_name() {
        let mut status = empty_status("feature/very-long-branch");
        status.worktree_info.push(worktree_entry("wt"));
        let entry = entry_with_status("/r", status);

        let cols = indicator_columns(&entry, 0);
        // 2 + 24 (branch chars) + 1 (space) = 27 → worktree start.
        let (w_start, _) = cols.worktree.expect("worktree range");
        assert_eq!(w_start, 27);
    }
}
