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
#[derive(Clone, Debug)]
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
                // Stash rows don't drive a separate detail view yet; the
                // parent repo stays selected for diff/graph routing.
                let id = RepoId(self.repos[*ri].path.clone());
                Some(Action::SelectRepo(id))
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
        if self.expanded_stashes.contains(&id) {
            self.expanded_stashes.remove(&id);
            self.rebuild_display_rows();
            self.select_repo_row(repo_idx);
        } else {
            self.expanded_stashes.insert(id);
            self.rebuild_display_rows();
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
        if self.expanded_repos.contains(&id) {
            // Collapsing: move selection to the parent repo row
            self.expanded_repos.remove(&id);
            self.rebuild_display_rows();
            self.select_repo_row(repo_idx);
        } else {
            self.expanded_repos.insert(id);
            self.rebuild_display_rows();
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
        let mut spans = vec![
            Span::styled(
                "    \u{2387} ",
                Style::default().fg(t.worktree_subtree_icon),
            ),
            Span::styled(
                format!("{} ", wt.branch),
                Style::default().fg(t.worktree_subtree_branch),
            ),
        ];
        if wt.stash_count > 0 {
            spans.push(Span::styled(
                format!("${} ", wt.stash_count),
                Style::default().fg(t.stash),
            ));
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
                        // Click on already-selected repo row toggles a subtree. Worktrees
                        // take priority when both are present; stash falls through when
                        // the repo has no worktrees but does have stashes.
                        if self.state.selected() == Some(idx)
                            && let Some(DisplayRow::Repo(i)) = self.display_rows.get(idx)
                            && let Some(status) = self.repos[*i].status.as_ref()
                        {
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
