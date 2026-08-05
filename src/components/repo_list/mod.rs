use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{ListItem, ListState},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::git::status::RepoStatus;
use crate::repo_id::RepoId;
use crate::theme::Theme;

mod render;
#[cfg(test)]
mod tests;

/// Result of `RepoList::sync_paths` — the paths that newly appeared and the
/// paths that vanished. Empty `added` and `removed` mean the set is unchanged
/// and no rebuild ran.
#[derive(Default, Clone, Debug)]
pub(crate) struct SyncDiff {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

impl SyncDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RepoEntry {
    pub path: PathBuf,
    /// Basename of the repo directory (used by titles / menus / sort).
    pub name: String,
    /// Breadcrumb label shown in the repo list: the repo's path relative to
    /// its workspace root (basename when it lives outside every root). Keeps
    /// same-named repos distinguishable and makes the list read like the
    /// workspace tree.
    pub display: String,
    pub status: Option<RepoStatus>,
    /// True only during push/pull/rebase — shows animated spinner
    pub git_op: bool,
}

/// A resolved target for a context-menu git operation, produced by
/// [`RepoList::resolve_target`]. Decouples *where the op runs* (`exec_path`,
/// `branch`) from *which row tracks it* (`parent_index`), so the same handler
/// works for a top-level repo and for one of its linked worktrees.
pub(crate) struct OpTarget {
    pub exec_path: PathBuf,
    pub branch: String,
    pub parent_index: usize,
    pub ahead: usize,
    pub behind: usize,
    pub has_submodules: bool,
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
    /// Workspace roots used to compute each repo's relative display path
    /// (`display_path`). Needed at rescan time too, for repos added later.
    roots: Vec<PathBuf>,
    pub state: ListState,
    pub render_area: Rect,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    /// Which repos have their worktree list expanded
    expanded_repos: HashSet<RepoId>,
    /// Which repos have their stash list expanded
    expanded_stashes: HashSet<RepoId>,
    /// `(session, pane_cwd)` from the liveness probe; a repo/worktree whose path
    /// contains a pane cwd is "live" in that session.
    live_panes: Vec<(String, PathBuf)>,
    /// Computed mapping from visual row → data
    display_rows: Vec<DisplayRow>,
    theme: Arc<Theme>,
}

/// The label shown for a repo in the list: its path relative to the first
/// workspace root that contains it, falling back to the basename for repos
/// outside every root (e.g. pinned paths). The relative path instead of the
/// bare basename keeps same-named repos distinguishable and makes the list
/// read like the workspace tree.
pub(crate) fn display_path(path: &Path, roots: &[PathBuf]) -> String {
    for root in roots {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_string_lossy().to_string();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Width of the fixed status block rendered to the right of the repo display
/// name (branch, ahead/behind, stash/worktree toggles, submodule markers,
/// fetch warning, file count). Mirrors `render_repo_item`; used to budget the
/// name column so a long path never pushes the status indicators off-screen.
fn status_block_width(entry: &RepoEntry) -> u16 {
    let mut w: u16 = 0;
    // The dirty/git-op marker is rendered before the name, so it isn't part
    // of the trailing block; everything from the branch onward is.
    let Some(status) = entry.status.as_ref() else {
        return w;
    };
    // Branch is left-padded to a minimum of 12 columns, then a space.
    w += status.branch.chars().count().max(12) as u16 + 1;
    if status.ahead > 0 {
        w += 2 + status.ahead.to_string().len() as u16;
    }
    if status.behind > 0 {
        w += 2 + status.behind.to_string().len() as u16;
    }
    if !status.stashes.is_empty() {
        w += 3 + status.stash_count().to_string().len() as u16;
    }
    if !status.worktree_info.is_empty() {
        w += 2 + status.worktree_info.len().to_string().len() as u16;
    }
    if status.has_dirty_submodules {
        w += 3;
    }
    if status.has_unpushed_submodules {
        w += 3;
    }
    if status.fetch_failed {
        w += 3;
    }
    if !status.files.is_empty() {
        // "[N] "
        w += 2 + status.files.len().to_string().len() as u16 + 1;
    }
    w
}

/// The repo's display label for the list row, middle-ellipsized to fit the
/// panel. Reserves the leading dirty/git-op marker, a trailing liveness
/// marker, and the fixed status block so the row never clips its indicators.
fn rendered_name(entry: &RepoEntry, inner_width: u16) -> String {
    let max = inner_width
        .saturating_sub(2) // dirty/git-op marker
        .saturating_sub(2) // liveness marker
        .saturating_sub(1) // space after the name
        .saturating_sub(status_block_width(entry))
        .max(1);
    middle_ellipsize(&entry.display, max as usize)
}

/// Collapse an over-long path to "head/…/tail", keeping the top-level group
/// and the repo basename visible while hiding the middle of deep paths.
/// Falls back to "…/tail" / "head/…" and finally a hard tail-truncation.
fn middle_ellipsize(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let parts: Vec<&str> = s.split('/').collect();
    let head = parts.first().copied().unwrap_or(s);
    let tail = parts.last().copied().unwrap_or(s);
    let head_tail = format!("{head}/…/{tail}");
    if head_tail.chars().count() <= max {
        return head_tail;
    }
    let tail_only = format!("…/{tail}");
    if tail_only.chars().count() <= max {
        return tail_only;
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl RepoList {
    pub fn new(repo_paths: Vec<PathBuf>, roots: Vec<PathBuf>, theme: Arc<Theme>) -> Self {
        let repos: Vec<RepoEntry> = repo_paths
            .into_iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let display = display_path(&path, &roots);
                RepoEntry {
                    path,
                    name,
                    display,
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
            roots,
            state,
            render_area: Rect::default(),
            focused: true,
            action_tx: None,
            expanded_repos: HashSet::new(),
            expanded_stashes: HashSet::new(),
            live_panes: Vec::new(),
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

    /// Resolve a `RepoId` to a concrete git-operation target.
    ///
    /// The id may identify a top-level repo or one of its linked worktrees.
    /// `exec_path` is the working directory `git -C` runs in; `branch` is the
    /// branch checked out there. `parent_index` is the top-level repo whose
    /// row shows the spinner and whose status query refreshes the data — a
    /// worktree borrows its parent's row for progress feedback because its
    /// ahead/behind counts are re-read by the parent's status query. For a
    /// top-level repo the target is itself. Returns `None` if the path matches
    /// neither a repo nor any known worktree.
    pub fn resolve_target(&self, id: &RepoId) -> Option<OpTarget> {
        if let Some(i) = self.resolve_index(id) {
            let status = self.repos[i].status.as_ref();
            return Some(OpTarget {
                exec_path: self.repos[i].path.clone(),
                branch: status.map(|s| s.branch.clone()).unwrap_or_default(),
                parent_index: i,
                ahead: status.map_or(0, |s| s.ahead),
                behind: status.map_or(0, |s| s.behind),
                has_submodules: status.is_some_and(|s| s.has_submodules),
            });
        }
        for (i, entry) in self.repos.iter().enumerate() {
            let Some(status) = entry.status.as_ref() else {
                continue;
            };
            if let Some(wt) = status.worktree_info.iter().find(|w| w.path == id.0) {
                return Some(OpTarget {
                    exec_path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    parent_index: i,
                    ahead: wt.ahead,
                    behind: wt.behind,
                    // Submodule operations are not surfaced for worktrees.
                    has_submodules: false,
                });
            }
        }
        None
    }

    /// Returns the parent RepoEntry for the current selection.
    pub fn selected_repo(&self) -> Option<&RepoEntry> {
        self.selected_index().and_then(|i| self.repos.get(i))
    }

    /// If a worktree row is currently selected, returns the parent repo path
    /// and the worktree details. Returns None when a repo row is selected.
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

    /// Resolve a worktree row id to `(parent repo path, worktree path, branch)`,
    /// independent of the current selection. Used by the path-bound context-menu
    /// remove action.
    pub fn worktree_remove_target(&self, id: &RepoId) -> Option<(PathBuf, PathBuf, String)> {
        for entry in &self.repos {
            let Some(status) = entry.status.as_ref() else {
                continue;
            };
            if let Some(wt) = status.worktree_info.iter().find(|w| w.path == id.0) {
                return Some((entry.path.clone(), wt.path.clone(), wt.branch.clone()));
            }
        }
        None
    }

    /// The tmux pane sessions from the latest liveness probe.
    pub fn live_panes(&self) -> &[(String, PathBuf)] {
        &self.live_panes
    }

    /// Replace the tmux pane sessions used to mark live repos/worktrees.
    pub fn set_live_panes(&mut self, panes: Vec<(String, PathBuf)>) {
        self.live_panes = panes;
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
                let display = display_path(path, &self.roots);
                next.push(RepoEntry {
                    path: path.clone(),
                    name,
                    display,
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

        // The repo's display name anchors the left of the row: its path
        // relative to the workspace root, middle-ellipsized so deep paths
        // can't push the status indicators off-screen.
        let inner_width = self.render_area.width.saturating_sub(2);
        spans.push(Span::styled(
            rendered_name(entry, inner_width),
            Style::default().fg(t.repo_name),
        ));
        spans.push(Span::raw(" "));

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

        // Liveness marker at the end of the row so it never shifts the name
        // column (bare symbol; the session names are in the context menu).
        if crate::session::liveness::is_live(&entry.path, &self.live_panes) {
            spans.push(Span::styled(" \u{25c9}", Style::default().fg(t.live)));
        }

        ListItem::new(Line::from(spans))
    }

    fn render_worktree_item(&self, entry: &RepoEntry, wt_idx: usize) -> ListItem<'static> {
        let t = &self.theme.repo_list;
        let wt = &entry.status.as_ref().unwrap().worktree_info[wt_idx];
        let mut spans = vec![Span::styled(
            "    \u{2387} ",
            Style::default().fg(t.worktree_subtree_icon),
        )];

        if wt.is_dirty {
            spans.push(Span::styled("* ", Style::default().fg(t.dirty_marker)));
        } else {
            spans.push(Span::raw("  "));
        }

        spans.push(Span::styled(
            format!("{:<12} ", wt.branch),
            Style::default().fg(t.worktree_subtree_branch),
        ));

        if wt.ahead > 0 {
            spans.push(Span::styled(
                format!("\u{2191}{} ", wt.ahead),
                Style::default().fg(t.ahead),
            ));
        }
        if wt.behind > 0 {
            spans.push(Span::styled(
                format!("\u{2193}{} ", wt.behind),
                Style::default().fg(t.behind),
            ));
        }

        if wt.has_dirty_submodules {
            spans.push(Span::styled(
                "\u{25c8} ",
                Style::default().fg(t.dirty_submodule),
            ));
        }

        if wt.has_unpushed_submodules {
            spans.push(Span::styled(
                "\u{21e1} ",
                Style::default().fg(t.unpushed_submodule),
            ));
        }

        if wt.file_count > 0 {
            spans.push(Span::styled(
                format!("[{}] ", wt.file_count),
                Style::default().fg(t.file_count),
            ));
        }

        if crate::session::liveness::is_live(&wt.path, &self.live_panes) {
            spans.push(Span::styled("\u{25c9} ", Style::default().fg(t.live)));
        }

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
