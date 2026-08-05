use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{layout::Rect, widgets::ListState};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::git::graph::{BranchSegment, GraphBuilder, GraphFilters, GraphOptions, GraphRow};
use crate::theme::Theme;

mod component;
#[cfg(test)]
mod tests;

/// RAII guard that guarantees the graph's `load_in_flight` latch is released
/// even when the background build panics. The build runs in a fire-and-forget
/// `spawn_blocking` whose `JoinHandle` is never awaited, so a panic in
/// `GraphBuilder::build` would otherwise swallow both the success and error
/// paths and strand `load_in_flight = true` — freezing the graph for that repo
/// until the user switches away and back. On drop without `complete()`, it
/// emits `GraphLoadAborted` so the latch is cleared for its generation.
struct GraphLoadGuard {
    generation: u64,
    tx: UnboundedSender<Action>,
    completed: bool,
}

impl GraphLoadGuard {
    fn new(generation: u64, tx: UnboundedSender<Action>) -> Self {
        Self {
            generation,
            tx,
            completed: false,
        }
    }

    /// Mark the build as having reported a terminal result so `Drop` is a no-op.
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for GraphLoadGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.tx.send(Action::GraphLoadAborted {
                generation: self.generation,
            });
        }
    }
}

struct CommitDetail {
    oid: String,
    message: String,
    files: Vec<(String, String)>,
    file_state: ListState,
    diff_content: Option<String>,
    diff_scroll: u16,
    /// True while the diff pane owns the keyboard, so `j`/`k` scroll the diff
    /// instead of moving the file highlight. The diff shows itself as soon as a
    /// file is highlighted, so its presence alone must not capture the keys;
    /// only `Enter` on a file row or a click inside the diff focuses it.
    diff_focused: bool,
    msg_scroll: u16,
    /// Rendered rect for the commit message block (set during draw).
    msg_area: Rect,
    /// Rendered rect for the file list block (set during draw).
    file_list_area: Rect,
}

struct SearchState {
    visible: bool,
    input: String,
    matches: Vec<usize>,
    current_match: Option<usize>,
}

impl SearchState {
    fn new() -> Self {
        Self {
            visible: false,
            input: String::new(),
            matches: Vec::new(),
            current_match: None,
        }
    }

    fn clear(&mut self) {
        self.visible = false;
        self.input.clear();
        self.matches.clear();
        self.current_match = None;
    }
}

/// How many consecutive aborted (panicked) builds to auto-retry before giving
/// up and surfacing an error. Keeps a deterministically-failing build from
/// replaying indefinitely while still self-healing from a one-off panic.
const MAX_CONSECUTIVE_ABORTS: u8 = 2;

pub(crate) struct GitGraph {
    /// Display rows (may contain collapsed placeholders).
    rows: Vec<GraphRow>,
    /// Rows from the current graph build, before branch-collapse display logic.
    all_rows: Vec<GraphRow>,
    /// Known filter values for the current repository. They accumulate across
    /// filtered reloads so deselected values remain available to re-enable.
    filter_branches: BTreeSet<String>,
    filter_authors: BTreeSet<String>,
    /// Branches currently collapsed in the view.
    collapsed_branches: std::collections::HashSet<String>,
    /// DAG-computed branch segments (non-trunk groups of commits).
    segments: Vec<BranchSegment>,
    /// Maps all_rows index → segment index (None = main trunk).
    row_to_segment: Vec<Option<usize>>,
    state: ListState,
    repo_name: String,
    repo_path: Option<PathBuf>,
    loading: bool,
    error: Option<String>,
    pub focused: bool,
    action_tx: Option<UnboundedSender<Action>>,
    render_area: Rect,
    graph_list_area: Rect,
    files_area: Rect,
    diff_area: Rect,
    commit_detail: Option<CommitDetail>,
    pub(crate) graph_options: GraphOptions,
    search: SearchState,
    /// Horizontal scroll offset (characters) for the graph list
    h_scroll: usize,
    pub horizontal_layout: bool,
    /// Deferred reload: set when graph data arrives while detail is open.
    needs_reload: bool,
    /// True while a graph rebuild is running for the current repo.
    load_in_flight: bool,
    /// Consecutive aborted (panicked) builds since the last successful load.
    /// Bounds the auto-retry so a deterministically-panicking build can't
    /// replay forever under a watcher event storm.
    consecutive_aborts: u8,
    /// Monotonic counter to discard stale GraphLoaded/DiffStatsLoaded results.
    load_generation: u64,
    /// Monotonic counter to discard stale CommitFilesLoaded/CommitDiffLoaded results.
    detail_generation: u64,
    theme: Arc<Theme>,
}

impl GitGraph {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            rows: Vec::new(),
            all_rows: Vec::new(),
            filter_branches: BTreeSet::new(),
            filter_authors: BTreeSet::new(),
            collapsed_branches: std::collections::HashSet::new(),
            segments: Vec::new(),
            row_to_segment: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
            repo_path: None,
            loading: false,
            error: None,
            focused: false,
            action_tx: None,
            render_area: Rect::default(),
            graph_list_area: Rect::default(),
            files_area: Rect::default(),
            diff_area: Rect::default(),
            commit_detail: None,
            graph_options: GraphOptions::default(),
            search: SearchState::new(),
            h_scroll: 0,
            horizontal_layout: false,
            needs_reload: false,
            load_in_flight: false,
            consecutive_aborts: 0,
            load_generation: 0,
            detail_generation: 0,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn load_repo(&mut self, path: PathBuf, repo_name: &str) {
        let is_same_repo = self.repo_path.as_deref() == Some(path.as_path());

        if is_same_repo && self.load_in_flight {
            self.needs_reload = true;
            return;
        }

        self.repo_name = repo_name.to_string();
        self.repo_path = Some(path.clone());
        self.error = None;

        // Keep old rows visible during reload (prevents blinking).
        // Only clear on repo switch.
        if !is_same_repo {
            self.loading = true;
            self.rows.clear();
            self.all_rows.clear();
            self.state.select(None);
            self.commit_detail = None;
            self.needs_reload = false;
            self.consecutive_aborts = 0;
            self.search.clear();
            self.filter_branches.clear();
            self.filter_authors.clear();
            self.collapsed_branches.clear();
            self.segments.clear();
            self.row_to_segment.clear();
        }

        let Some(tx) = &self.action_tx else { return };
        let tx = tx.clone();
        let options = self.graph_options.clone();
        self.load_in_flight = true;
        self.load_generation += 1;
        let load_gen = self.load_generation;

        tokio::task::spawn_blocking(move || {
            // Releases `load_in_flight` via `GraphLoadAborted` if `build` panics
            // before either terminal action below is sent.
            let guard = GraphLoadGuard::new(load_gen, tx.clone());
            let builder = GraphBuilder::new();
            if let Ok(branches) = GraphBuilder::branch_names(&path, &options.branch_filter) {
                let _ = tx.send(Action::GraphFilterBranchesLoaded {
                    generation: load_gen,
                    branches,
                });
            }
            match builder.build(&path, &options) {
                Ok(rows) => {
                    let oids: Vec<git2::Oid> = rows.iter().map(|r| r.oid).collect();
                    let _ = tx.send(Action::GraphLoaded {
                        generation: load_gen,
                        rows,
                    });
                    // `GraphLoaded` is the terminal action for the latch, so
                    // mark complete before the optional (and separately
                    // fallible) stats pass — a panic in `batch_diff_stats` must
                    // not emit a false `GraphLoadAborted` for an already-loaded
                    // graph.
                    guard.complete();
                    // Compute stats after graph is sent — graph appears instantly
                    if options.show_stats
                        && let Ok(stats) = crate::git::commit_files::batch_diff_stats(&path, &oids)
                    {
                        let _ = tx.send(Action::DiffStatsLoaded {
                            generation: load_gen,
                            stats,
                        });
                    }
                }
                Err(e) => {
                    let _ = tx.send(Action::GraphError {
                        generation: load_gen,
                        message: format!("Failed to load graph: {}", e),
                    });
                    guard.complete();
                }
            }
        });
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        self.loading = false;
        self.load_in_flight = false;
    }

    pub fn set_rows(&mut self, mut rows: Vec<GraphRow>) {
        // Preserve selection position on refresh if possible
        let prev_selected = self.state.selected();
        // Carry forward diff_stats from previous all_rows to avoid blink on refresh
        if !self.all_rows.is_empty() {
            let old_stats: std::collections::HashMap<git2::Oid, crate::git::graph::DiffStat> = self
                .all_rows
                .iter()
                .filter_map(|r| r.diff_stat.clone().map(|s| (r.oid, s)))
                .collect();
            for row in &mut rows {
                if row.diff_stat.is_none() {
                    row.diff_stat = old_stats.get(&row.oid).cloned();
                }
            }
        }
        self.filter_branches.extend(
            rows.iter()
                .flat_map(|row| row.labels.iter())
                .filter(|label| !label.is_tag && !label.is_stash)
                .map(|label| label.name.clone()),
        );
        self.filter_authors
            .extend(rows.iter().map(|row| row.author.clone()));
        self.all_rows = rows;
        self.loading = false;
        self.load_in_flight = false;
        self.consecutive_aborts = 0;
        self.recompute_segments();
        self.recompute_collapsed_rows();
        if !self.display_rows().is_empty() {
            let idx = prev_selected
                .map(|i| i.min(self.display_rows().len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        }

        if std::mem::take(&mut self.needs_reload) {
            self.reload_graph();
        }
    }

    pub fn set_filter_branches(&mut self, branches: Vec<String>) {
        self.filter_branches.extend(branches);
    }

    pub fn set_diff_stats(&mut self, stats: Vec<(git2::Oid, crate::git::graph::DiffStat)>) {
        let stat_map: std::collections::HashMap<_, _> = stats.into_iter().collect();
        for row in &mut self.all_rows {
            if let Some(stat) = stat_map.get(&row.oid) {
                row.diff_stat = Some(stat.clone());
            }
        }
        self.recompute_collapsed_rows();
    }

    /// Open the detail pane for a loaded commit. Returns the request for the
    /// first file's diff so the Diff pane fills in without a second keypress;
    /// `None` when the commit changed nothing.
    #[must_use]
    pub fn set_commit_files(
        &mut self,
        oid: String,
        message: String,
        files: Vec<(String, String)>,
    ) -> Option<Action> {
        let mut file_state = ListState::default();
        if !files.is_empty() {
            file_state.select(Some(0));
        }
        self.commit_detail = Some(CommitDetail {
            oid,
            message,
            files,
            file_state,
            diff_content: None,
            diff_scroll: 0,
            diff_focused: false,
            msg_scroll: 0,
            msg_area: Rect::default(),
            file_list_area: Rect::default(),
        });
        self.try_show_commit_diff()
    }

    pub fn set_commit_diff(&mut self, content: String) {
        if let Some(ref mut detail) = self.commit_detail {
            detail.diff_content = Some(content);
            detail.diff_scroll = 0;
        }
    }

    pub fn has_detail(&self) -> bool {
        self.commit_detail.is_some()
    }

    pub fn set_needs_reload(&mut self) {
        self.needs_reload = true;
    }

    /// Release the in-flight latch when a background build aborts without
    /// reporting (it panicked). Keeps the currently-displayed rows so the view
    /// doesn't blink, and honors a pending `needs_reload` so a refresh that was
    /// coalesced during the dead build still runs — but only up to
    /// `MAX_CONSECUTIVE_ABORTS`, after which it surfaces an error and stops
    /// auto-retrying so a deterministically-panicking build can't replay forever.
    pub fn abort_load(&mut self) {
        // Ignore a stale abort: a build that already reported `GraphLoaded`
        // (latch cleared) may still drop its guard if a later stats pass
        // panics. Without this, the abort would consume a fresh `needs_reload`
        // and fire a spurious reload for an already-loaded graph.
        if !self.load_in_flight {
            return;
        }
        self.load_in_flight = false;
        self.loading = false;
        self.consecutive_aborts = self.consecutive_aborts.saturating_add(1);

        if self.consecutive_aborts > MAX_CONSECUTIVE_ABORTS {
            // Drop the coalesced reload; replaying it would just panic again.
            // A fresh repo (re)selection resets the counter and lets it retry.
            self.needs_reload = false;
            self.error = Some("Graph build failed repeatedly".to_string());
            return;
        }

        if std::mem::take(&mut self.needs_reload) {
            self.reload_graph();
        }
    }

    pub fn current_generation(&self) -> u64 {
        self.load_generation
    }

    pub fn current_detail_generation(&self) -> u64 {
        self.detail_generation
    }

    pub fn filters(&self) -> GraphFilters {
        self.graph_options.filters.clone()
    }

    pub fn filter_branches(&self) -> Vec<String> {
        self.filter_branches.iter().cloned().collect()
    }

    pub fn filter_authors(&self) -> Vec<String> {
        self.filter_authors.iter().cloned().collect()
    }

    pub fn set_filters(&mut self, filters: GraphFilters) {
        if self.graph_options.filters == filters {
            return;
        }
        self.graph_options.filters = filters;
        self.collapsed_branches.clear();
        self.search.clear();
        self.reload_graph();
    }

    pub fn first_parent(&self) -> bool {
        self.graph_options.first_parent
    }

    pub fn selected_commit_menu_data(&self) -> Option<(String, String, String)> {
        let row = self.display_rows().get(self.state.selected()?)?;
        Some((
            row.oid.to_string(),
            row.short_id.clone(),
            row.message.clone(),
        ))
    }

    pub fn can_toggle_selected_branch(&self) -> bool {
        let Some(index) = self.state.selected() else {
            return false;
        };
        let Some(row) = self.display_rows().get(index) else {
            return false;
        };
        if row.collapsed.is_some() {
            return true;
        }
        self.all_rows
            .iter()
            .position(|candidate| candidate.oid == row.oid)
            .and_then(|index| self.row_to_segment.get(index))
            .is_some_and(Option::is_some)
    }

    pub fn open_selected_commit_files(&mut self) -> Option<Action> {
        self.try_show_commit_files()
    }

    pub fn open_search(&mut self) {
        self.search.visible = true;
        self.search.input.clear();
        self.search.matches.clear();
        self.search.current_match = None;
    }

    pub fn toggle_selected_branch(&mut self) {
        self.toggle_collapse_selected();
    }

    pub fn expand_all(&mut self) {
        self.expand_all_branches();
    }

    pub fn set_first_parent(&mut self, enabled: bool) {
        if self.graph_options.first_parent != enabled {
            self.graph_options.first_parent = enabled;
            self.reload_graph();
        }
    }

    /// Toggle collapse on the selected row's branch (or expand a collapsed group).
    fn toggle_collapse_selected(&mut self) {
        let Some(idx) = self.state.selected() else {
            return;
        };
        let Some(row) = self.display_rows().get(idx) else {
            return;
        };

        // Extract data before dropping the borrow on self
        let collapsed_key = row.collapsed.as_ref().map(|(k, _)| k.clone());
        let row_oid = row.oid;

        // If this is a collapsed placeholder, expand it
        if let Some(key) = collapsed_key {
            self.collapsed_branches.remove(key.as_str());
            self.recompute_collapsed_rows();
            return;
        }

        // Find this row in all_rows and look up its segment
        let Some(all_idx) = self.all_rows.iter().position(|r| r.oid == row_oid) else {
            return;
        };
        let Some(Some(seg_idx)) = self.row_to_segment.get(all_idx) else {
            return; // Main trunk — not collapsible
        };
        let seg = &self.segments[*seg_idx];
        self.collapsed_branches.insert(seg.id.clone());
        self.recompute_collapsed_rows();
    }

    /// Expand all collapsed branches.
    fn expand_all_branches(&mut self) {
        if self.collapsed_branches.is_empty() {
            return;
        }
        self.collapsed_branches.clear();
        self.recompute_collapsed_rows();
    }

    fn reload_graph(&mut self) {
        if let Some(path) = self.repo_path.clone() {
            let name = self.repo_name.clone();
            self.load_repo(path, &name);
        }
    }

    /// Recompute segments and row_to_segment mapping from all_rows.
    fn recompute_segments(&mut self) {
        self.segments = crate::git::graph::compute_branch_segments(&self.all_rows);
        self.row_to_segment = vec![None; self.all_rows.len()];
        for (seg_idx, seg) in self.segments.iter().enumerate() {
            for &row_idx in &seg.row_indices {
                self.row_to_segment[row_idx] = Some(seg_idx);
            }
        }
    }

    /// Returns the appropriate row slice for read-only access.
    /// When no branches are collapsed, reads directly from `all_rows`
    /// to avoid an unnecessary clone.
    fn display_rows(&self) -> &[GraphRow] {
        if self.collapsed_branches.is_empty() {
            &self.all_rows
        } else {
            &self.rows
        }
    }

    /// Recompute `self.rows` from `self.all_rows`, collapsing groups.
    fn recompute_collapsed_rows(&mut self) {
        if self.collapsed_branches.is_empty() {
            self.rows.clear();
            return;
        }

        // Collect all hidden row indices and prepare placeholders
        let mut hidden: std::collections::HashSet<usize> = std::collections::HashSet::new();
        // (tip_row_idx, segment_id, display_name, count)
        let mut placeholders: Vec<(usize, String, String, usize)> = Vec::new();

        for seg in &self.segments {
            if !self.collapsed_branches.contains(&seg.id) {
                continue;
            }
            for &row_idx in &seg.row_indices {
                hidden.insert(row_idx);
            }
            let tip_idx = seg.row_indices[0];
            placeholders.push((
                tip_idx,
                seg.id.clone(),
                seg.display_name.clone(),
                seg.row_indices.len(),
            ));
        }

        let mut rows = Vec::new();
        for (i, row) in self.all_rows.iter().enumerate() {
            if hidden.contains(&i) {
                if let Some((_, seg_id, name, count)) =
                    placeholders.iter().find(|(tip, _, _, _)| *tip == i)
                {
                    let mut placeholder = row.clone();
                    placeholder.message = format!("\u{25b6} {name} ({count} commits)");
                    placeholder.short_id = String::new();
                    placeholder.author = String::new();
                    placeholder.labels = Vec::new();
                    placeholder.diff_stat = None;
                    placeholder.collapsed = Some((seg_id.clone(), *count));
                    rows.push(placeholder);
                }
                continue;
            }
            rows.push(row.clone());
        }

        self.rows = rows;
    }

    pub fn selected_text(&self) -> Option<String> {
        // If viewing commit files, copy the selected file path
        if let Some(ref detail) = self.commit_detail
            && let Some(idx) = detail.file_state.selected()
            && let Some((_, path)) = detail.files.get(idx)
        {
            return Some(path.clone());
        }
        // Otherwise copy the selected commit's short id + message
        let idx = self.state.selected()?;
        let row = self.display_rows().get(idx)?;
        Some(format!("{} {}", row.short_id, row.message))
    }

    pub fn search_visible(&self) -> bool {
        self.search.visible
    }

    pub fn handle_search_key(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Esc => {
                self.search.visible = false;
            }
            KeyCode::Enter => {
                self.search.visible = false;
                // Jump to first match if any
                if let Some(&idx) = self.search.matches.first() {
                    self.search.current_match = Some(0);
                    self.state.select(Some(idx));
                }
            }
            KeyCode::Backspace => {
                self.search.input.pop();
                self.update_search_matches();
            }
            KeyCode::Char(c) => {
                self.search.input.push(c);
                self.update_search_matches();
            }
            _ => {}
        }
        Ok(None)
    }

    fn update_search_matches(&mut self) {
        self.search.current_match = None;
        if self.search.input.is_empty() {
            self.search.matches.clear();
            return;
        }
        let query = self.search.input.to_lowercase();
        let matches: Vec<usize> = self
            .display_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.message.to_lowercase().contains(&query)
                    || row.author.to_lowercase().contains(&query)
                    || row.short_id.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if !matches.is_empty() {
            self.search.current_match = Some(0);
        }
        self.search.matches = matches;
    }

    fn search_next(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let next = match self.search.current_match {
            Some(i) => (i + 1) % self.search.matches.len(),
            None => 0,
        };
        self.search.current_match = Some(next);
        self.state.select(Some(self.search.matches[next]));
    }

    fn search_prev(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let prev = match self.search.current_match {
            Some(0) | None => self.search.matches.len() - 1,
            Some(i) => i - 1,
        };
        self.search.current_match = Some(prev);
        self.state.select(Some(self.search.matches[prev]));
    }

    fn select_next(&mut self) {
        if self.display_rows().is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.display_rows().len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.display_rows().is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn try_show_commit_files(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        let oid = self.display_rows().get(idx)?.oid.to_string();
        let repo_path = self.repo_path.clone()?;
        self.detail_generation += 1;
        Some(Action::ShowCommitFiles { repo_path, oid })
    }

    /// Whether the detail pane already shows the commit on display row `idx`.
    fn is_detail_open_for(&self, idx: usize) -> bool {
        let Some(detail) = self.commit_detail.as_ref() else {
            return false;
        };
        self.display_rows()
            .get(idx)
            .is_some_and(|row| row.oid.to_string() == detail.oid)
    }

    fn try_show_commit_diff(&mut self) -> Option<Action> {
        let detail = self.commit_detail.as_ref()?;
        let file_idx = detail.file_state.selected()?;
        let (_, file_path) = detail.files.get(file_idx)?;
        let repo_path = self.repo_path.clone()?;
        self.detail_generation += 1;
        Some(Action::ShowCommitDiff {
            repo_path,
            oid: detail.oid.clone(),
            file_path: file_path.clone(),
        })
    }
}
