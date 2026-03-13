use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::graph::{BranchSegment, GraphBuilder, GraphOptions, GraphRow};
use crate::git::graph_render;

struct CommitDetail {
    oid: String,
    files: Vec<(String, String)>,
    file_state: ListState,
    diff_content: Option<String>,
    diff_scroll: u16,
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

pub(crate) struct GitGraph {
    /// Display rows (may contain collapsed placeholders).
    rows: Vec<GraphRow>,
    /// Full rows from the graph builder (never filtered).
    all_rows: Vec<GraphRow>,
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
}

impl GitGraph {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            all_rows: Vec::new(),
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
        }
    }

    pub fn load_repo(&mut self, path: PathBuf, repo_name: &str) {
        let is_same_repo = self.repo_path.as_deref() == Some(path.as_path());

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
            self.search.clear();
            self.collapsed_branches.clear();
            self.segments.clear();
            self.row_to_segment.clear();
        }

        let Some(tx) = &self.action_tx else { return };
        let tx = tx.clone();
        let options = self.graph_options.clone();

        tokio::task::spawn_blocking(move || {
            let builder = GraphBuilder::new();
            match builder.build(&path, &options) {
                Ok(rows) => {
                    let oids: Vec<git2::Oid> = rows.iter().map(|r| r.oid).collect();
                    let _ = tx.send(Action::GraphLoaded(rows));
                    // Compute stats after graph is sent — graph appears instantly
                    if options.show_stats
                        && let Ok(stats) = crate::git::commit_files::batch_diff_stats(&path, &oids)
                    {
                        let _ = tx.send(Action::DiffStatsLoaded(stats));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Action::GraphError(format!("Failed to load graph: {}", e)));
                }
            }
        });
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        self.loading = false;
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
        self.all_rows = rows;
        self.loading = false;
        self.recompute_segments();
        self.recompute_collapsed_rows();
        if !self.display_rows().is_empty() {
            let idx = prev_selected
                .map(|i| i.min(self.display_rows().len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        }
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

    pub fn set_commit_files(&mut self, oid: String, files: Vec<(String, String)>) {
        let mut file_state = ListState::default();
        if !files.is_empty() {
            file_state.select(Some(0));
        }
        self.commit_detail = Some(CommitDetail {
            oid,
            files,
            file_state,
            diff_content: None,
            diff_scroll: 0,
        });
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

    fn try_show_commit_files(&self) -> Option<Action> {
        let idx = self.state.selected()?;
        let row = self.display_rows().get(idx)?;
        let repo_path = self.repo_path.clone()?;
        Some(Action::ShowCommitFiles {
            repo_path,
            oid: row.oid.to_string(),
        })
    }

    fn try_show_commit_diff(&self) -> Option<Action> {
        let detail = self.commit_detail.as_ref()?;
        let file_idx = detail.file_state.selected()?;
        let (_, file_path) = detail.files.get(file_idx)?;
        let repo_path = self.repo_path.clone()?;
        Some(Action::ShowCommitDiff {
            repo_path,
            oid: detail.oid.clone(),
            file_path: file_path.clone(),
        })
    }

    fn draw_graph_list(&mut self, frame: &mut Frame, area: Rect) {
        let collapsed_count = self.collapsed_branches.len();
        let title = match (self.graph_options.first_parent, collapsed_count) {
            (true, 0) => format!(" Git Graph — {} [1st-parent] ", self.repo_name),
            (true, n) => format!(
                " Git Graph — {} [1st-parent] ({n} collapsed) ",
                self.repo_name
            ),
            (false, 0) => format!(" Git Graph — {} ", self.repo_name),
            (false, n) => format!(" Git Graph — {} ({n} collapsed) ", self.repo_name),
        };
        let border_color = if self.focused && self.commit_detail.is_none() {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.loading {
            let paragraph = Paragraph::new("Loading graph...")
                .style(Style::default().fg(Color::Yellow))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if let Some(ref err) = self.error {
            let paragraph = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        if self.display_rows().is_empty() {
            let paragraph = Paragraph::new("No commits")
                .style(Style::default().fg(Color::Gray))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let label_max_len = self.graph_options.label_max_len;
        let max_width = area.width.saturating_sub(2) as usize; // 2 for borders
        let has_search = !self.search.input.is_empty() && !self.search.matches.is_empty();
        let items: Vec<ListItem> = self
            .display_rows()
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let dimmed = has_search && !self.search.matches.contains(&i);
                let is_collapsed = row.collapsed.is_some();
                let mut spans = graph_render::render_graph_prefix(row);

                if dimmed || is_collapsed {
                    for span in &mut spans {
                        span.style = Style::default().fg(Color::DarkGray);
                    }
                }

                if is_collapsed {
                    // Collapsed placeholder: show only the message with italic style
                    spans.push(Span::styled(
                        row.message.clone(),
                        Style::default()
                            .fg(Color::Rgb(130, 130, 130))
                            .add_modifier(Modifier::ITALIC),
                    ));
                } else {
                    let id_style = if dimmed {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    };
                    spans.push(Span::styled(format!("{} ", row.short_id), id_style));

                    if !dimmed {
                        spans.extend(graph_render::render_branch_labels(
                            &row.labels,
                            label_max_len,
                        ));
                    }

                    let msg_color = if dimmed {
                        Color::DarkGray
                    } else if row.is_merge {
                        Color::Rgb(130, 130, 130)
                    } else {
                        Color::White
                    };
                    spans.push(Span::styled(
                        row.message.clone(),
                        Style::default().fg(msg_color),
                    ));

                    let author_color = if dimmed {
                        Color::DarkGray
                    } else {
                        graph_render::author_color(&row.author)
                    };
                    spans.push(Span::styled(
                        format!("  — {}", row.author),
                        Style::default().fg(author_color),
                    ));
                    spans.push(Span::styled(
                        format!(" {}", graph_render::format_relative_time(row.time)),
                        Style::default().fg(Color::DarkGray),
                    ));

                    if let Some(ref stat) = row.diff_stat
                        && !dimmed
                    {
                        if stat.additions > 0 {
                            spans.push(Span::styled(
                                format!(" +{}", stat.additions),
                                Style::default().fg(Color::Green),
                            ));
                        }
                        if stat.deletions > 0 {
                            spans.push(Span::styled(
                                format!(" -{}", stat.deletions),
                                Style::default().fg(Color::Red),
                            ));
                        }
                    }
                }

                graph_render::h_scroll_line(&mut spans, self.h_scroll, max_width);
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_commit_files(detail: &mut CommitDetail, frame: &mut Frame, area: Rect) {
        let title = format!(" Files — {} ", &detail.oid[..7.min(detail.oid.len())]);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        if detail.files.is_empty() {
            let paragraph = Paragraph::new("No files changed")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = detail
            .files
            .iter()
            .map(|(status, path)| {
                let color = match status.as_str() {
                    "M" => Color::Yellow,
                    "A" => Color::Green,
                    "D" => Color::Red,
                    "R" => Color::Blue,
                    _ => Color::DarkGray,
                };
                let spans = vec![
                    Span::styled(
                        format!(" {} ", status),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(path, Style::default().fg(Color::White)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut detail.file_state);
    }

    fn draw_commit_diff(detail: &CommitDetail, frame: &mut Frame, area: Rect) {
        let Some(ref content) = detail.diff_content else {
            return;
        };

        let block = Block::default()
            .title(" Commit Diff (Esc to close) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let lines: Vec<Line> = content
            .lines()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("@@") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
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

impl Component for GitGraph {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        // When detail is open, Esc/keys are layered
        if let Some(ref mut detail) = self.commit_detail {
            if detail.diff_content.is_some() {
                // Viewing commit diff
                match key.code {
                    KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => {
                        detail.diff_content = None;
                        detail.diff_scroll = 0;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        detail.diff_scroll = detail.diff_scroll.saturating_add(1);
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
                    return Ok(None);
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
                    return Ok(None);
                }
                KeyCode::Enter => {
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
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);

                // Click in graph list area
                if self.graph_list_area.contains(pos) {
                    let content_y = self.graph_list_area.y + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + self.state.offset();
                        if idx < self.display_rows().len() {
                            // Click on already-selected row opens commit files
                            if self.state.selected() == Some(idx) && self.commit_detail.is_none() {
                                return Ok(self.try_show_commit_files());
                            }
                            self.state.select(Some(idx));
                            self.commit_detail = None;
                        }
                    }
                    return Ok(None);
                }

                // Click in commit files area
                let mut open_file_diff = false;
                if let Some(ref mut detail) = self.commit_detail
                    && self.files_area.contains(pos)
                {
                    let content_y = self.files_area.y + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + detail.file_state.offset();
                        if idx < detail.files.len() {
                            if detail.file_state.selected() == Some(idx) {
                                open_file_diff = true;
                            } else {
                                detail.file_state.select(Some(idx));
                            }
                        }
                    }
                }
                if open_file_diff {
                    return Ok(self.try_show_commit_diff());
                }

                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_sub(1);
                        return Ok(None);
                    }
                    if self.files_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| i.saturating_sub(1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        return Ok(None);
                    }
                }
                self.select_prev();
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if let Some(ref mut detail) = self.commit_detail {
                    if self.diff_area.contains(pos) && detail.diff_content.is_some() {
                        detail.diff_scroll = detail.diff_scroll.saturating_add(1);
                        return Ok(None);
                    }
                    if self.files_area.contains(pos) && !detail.files.is_empty() {
                        let i = detail
                            .file_state
                            .selected()
                            .map(|i| (i + 1).min(detail.files.len() - 1))
                            .unwrap_or(0);
                        detail.file_state.select(Some(i));
                        return Ok(None);
                    }
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
            MouseEventKind::Down(MouseButton::Right) => Ok(None),
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;

        match &self.commit_detail {
            Some(detail) if detail.diff_content.is_some() => {
                // Graph 40% | Files 25% | Diff 35%
                let dir = if self.horizontal_layout { Direction::Vertical } else { Direction::Horizontal };
                let chunks = Layout::default()
                    .direction(dir)
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Percentage(25),
                        Constraint::Percentage(35),
                    ])
                    .split(area);

                self.graph_list_area = chunks[0];
                self.files_area = chunks[1];
                self.diff_area = chunks[2];

                self.draw_graph_list(frame, chunks[0]);
                // Borrow detail mutably for drawing
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1]);
                Self::draw_commit_diff(detail, frame, chunks[2]);
            }
            Some(_) => {
                // Graph 50% | Files 50%
                let dir = if self.horizontal_layout { Direction::Vertical } else { Direction::Horizontal };
                let chunks = Layout::default()
                    .direction(dir)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(area);

                self.graph_list_area = chunks[0];
                self.files_area = chunks[1];
                self.diff_area = Rect::default();

                self.draw_graph_list(frame, chunks[0]);
                let detail = self.commit_detail.as_mut().unwrap();
                Self::draw_commit_files(detail, frame, chunks[1]);
            }
            None => {
                self.graph_list_area = area;
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
            let overlay = Paragraph::new(overlay_text)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(overlay, overlay_area);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::graph::{BranchLabel, GraphRow, LaneSegment};
    use git2::Oid;

    fn mock_row(short_id: &str, message: &str, author: &str) -> GraphRow {
        GraphRow {
            commit_col: 0,
            lanes: vec![LaneSegment::Commit],
            horizontal_spans: Vec::new(),
            oid: Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            short_id: short_id.to_string(),
            message: message.to_string(),
            author: author.to_string(),
            time: 0,
            labels: Vec::new(),
            is_merge: false,
            parent_oids: Vec::new(),
            diff_stat: None,
            collapsed: None,
        }
    }

    #[test]
    fn test_search_matches_message() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            mock_row("abc1234", "fix: resolve crash", "Alice"),
            mock_row("def5678", "feat: add login", "Bob"),
            mock_row("ghi9012", "chore: update deps", "Alice"),
        ]);

        graph.search.input = "login".to_string();
        graph.update_search_matches();

        assert_eq!(graph.search.matches, vec![1]);
    }

    #[test]
    fn test_search_matches_author() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            mock_row("abc1234", "first", "Alice"),
            mock_row("def5678", "second", "Bob"),
            mock_row("ghi9012", "third", "Alice"),
        ]);

        graph.search.input = "alice".to_string();
        graph.update_search_matches();

        assert_eq!(graph.search.matches, vec![0, 2]);
    }

    #[test]
    fn test_search_matches_short_id() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            mock_row("abc1234", "first", "Alice"),
            mock_row("def5678", "second", "Bob"),
        ]);

        graph.search.input = "def".to_string();
        graph.update_search_matches();

        assert_eq!(graph.search.matches, vec![1]);
    }

    #[test]
    fn test_search_case_insensitive() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![mock_row("abc1234", "Fix Bug", "Alice")]);

        graph.search.input = "fix bug".to_string();
        graph.update_search_matches();

        assert_eq!(graph.search.matches, vec![0]);
    }

    #[test]
    fn test_search_next_wraps_around() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            mock_row("a", "match", "X"),
            mock_row("b", "no", "Y"),
            mock_row("c", "match", "Z"),
        ]);

        graph.search.input = "match".to_string();
        graph.update_search_matches();

        // matches = [0, 2]
        assert_eq!(graph.search.current_match, Some(0));

        graph.search_next();
        assert_eq!(graph.search.current_match, Some(1));
        assert_eq!(graph.state.selected(), Some(2)); // row index 2

        graph.search_next();
        assert_eq!(graph.search.current_match, Some(0)); // wraps
        assert_eq!(graph.state.selected(), Some(0));
    }

    #[test]
    fn test_search_prev_wraps_around() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            mock_row("a", "match", "X"),
            mock_row("b", "no", "Y"),
            mock_row("c", "match", "Z"),
        ]);

        graph.search.input = "match".to_string();
        graph.update_search_matches();

        // Start at match 0
        graph.search_prev();
        assert_eq!(graph.search.current_match, Some(1)); // wraps to last
        assert_eq!(graph.state.selected(), Some(2));
    }

    #[test]
    fn test_search_empty_input_no_matches() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![mock_row("a", "hello", "X")]);

        graph.search.input.clear();
        graph.update_search_matches();

        assert!(graph.search.matches.is_empty());
        assert_eq!(graph.search.current_match, None);
    }

    #[test]
    fn test_search_no_results() {
        let mut graph = GitGraph::new();
        graph.set_rows(vec![mock_row("a", "hello", "Alice")]);

        graph.search.input = "zzzzz".to_string();
        graph.update_search_matches();

        assert!(graph.search.matches.is_empty());
        assert_eq!(graph.search.current_match, None);
    }

    fn make_label(name: &str) -> BranchLabel {
        BranchLabel {
            name: name.to_string(),
            is_head: false,
            is_remote: false,
            is_worktree: false,
            is_tag: false,
        }
    }

    const OID_M: &str = "1111111111111111111111111111111111111111";
    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OID_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    /// Build a DAG-wired row. `oid_str` must be valid hex.
    fn dag_row(
        oid_str: &str,
        short_id: &str,
        parent_oids: Vec<Oid>,
        col: usize,
        labels: Vec<BranchLabel>,
    ) -> GraphRow {
        GraphRow {
            commit_col: col,
            lanes: vec![LaneSegment::Commit],
            horizontal_spans: Vec::new(),
            oid: Oid::from_str(oid_str).unwrap(),
            short_id: short_id.to_string(),
            message: format!("msg-{short_id}"),
            author: "Author".to_string(),
            time: 0,
            labels,
            is_merge: parent_oids.len() > 1,
            parent_oids,
            diff_stat: None,
            collapsed: None,
        }
    }

    /// Standard topology for collapse tests:
    /// Row 0: main0 (col=0, parents=[], labels=["main"])  ← main trunk
    /// Row 1: tip   (col=1, parents=[mid], labels)         ← side branch tip
    /// Row 2: mid   (col=1, parents=[main0])               ← side branch base
    fn make_branch_rows(tip_labels: Vec<BranchLabel>) -> Vec<GraphRow> {
        let oid_m = Oid::from_str(OID_M).unwrap();
        let oid_b = Oid::from_str(OID_B).unwrap();

        vec![
            dag_row(OID_M, "m", vec![], 0, vec![make_label("main")]),
            dag_row(OID_A, "a", vec![oid_b], 1, tip_labels),
            dag_row(OID_B, "b", vec![oid_m], 1, vec![]),
        ]
    }

    #[test]
    fn test_collapse_labeled_branch() {
        let mut graph = GitGraph::new();
        graph.set_rows(make_branch_rows(vec![make_label("feature")]));
        // Select tip (row 1 in all_rows, row 1 in display)
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.contains(OID_A));
        // main0 + placeholder = 2 rows
        assert_eq!(graph.rows.len(), 2);
        let (_, count) = graph.rows[1].collapsed.as_ref().unwrap();
        assert_eq!(*count, 2);
        assert!(graph.rows[1].message.contains("feature"));
    }

    #[test]
    fn test_collapse_unlabeled_merge_lane() {
        let mut graph = GitGraph::new();
        // No labels on the side branch
        graph.set_rows(make_branch_rows(vec![]));
        graph.state.select(Some(2)); // select base row of side branch
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.contains(OID_A));
        assert_eq!(graph.rows.len(), 2);
        // Placeholder uses short OID since there's no label
        assert!(graph.rows[1].message.contains("a"));
    }

    #[test]
    fn test_expand_collapsed_group() {
        let mut graph = GitGraph::new();
        graph.set_rows(make_branch_rows(vec![make_label("feature")]));
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();
        assert_eq!(graph.rows.len(), 2);

        // Select the placeholder and toggle to expand
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.is_empty());
        assert_eq!(graph.display_rows().len(), 3);
    }

    #[test]
    fn test_collapse_from_middle_of_branch() {
        let mut graph = GitGraph::new();
        graph.set_rows(make_branch_rows(vec![make_label("feature")]));
        // Select the base row (row 2) — should collapse the whole segment
        graph.state.select(Some(2));
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.contains(OID_A));
        assert_eq!(graph.rows.len(), 2);
        assert!(graph.rows[1].collapsed.is_some());
    }

    #[test]
    fn test_expand_all() {
        let mut graph = GitGraph::new();
        graph.set_rows(make_branch_rows(vec![make_label("feat-a")]));
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();
        assert!(!graph.collapsed_branches.is_empty());

        graph.expand_all_branches();
        assert!(graph.collapsed_branches.is_empty());
        assert_eq!(graph.display_rows().len(), 3);
    }

    #[test]
    fn test_main_trunk_not_collapsible() {
        let mut graph = GitGraph::new();
        graph.set_rows(make_branch_rows(vec![]));
        // Select main trunk row (row 0)
        graph.state.select(Some(0));
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.is_empty());
        assert_eq!(graph.display_rows().len(), 3);
    }

    #[test]
    fn test_interleaved_commits_collapse_together() {
        // Row 0: main0 (col=0, parents=[main1])
        // Row 1: tip_x (col=1, parents=[base_x]) -- branch X
        // Row 2: main1 (col=0, parents=[])        -- main trunk
        // Row 3: base_x (col=1, parents=[main0])  -- branch X (interleaved with main1)
        let oid_m0 = Oid::from_str(OID_M).unwrap();
        let oid_b = Oid::from_str(OID_B).unwrap();
        let oid_c = Oid::from_str(OID_C).unwrap();

        let mut graph = GitGraph::new();
        graph.set_rows(vec![
            dag_row(OID_M, "m0", vec![oid_c], 0, vec![make_label("main")]),
            dag_row(OID_A, "a", vec![oid_b], 1, vec![]),
            dag_row(OID_C, "c", vec![], 0, vec![]),
            dag_row(OID_B, "b", vec![oid_m0], 1, vec![]),
        ]);

        // Select row 1 (tip of branch X)
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();

        assert!(graph.collapsed_branches.contains(OID_A));
        // Rows 1 and 3 (non-contiguous) should both be collapsed
        // main0 + placeholder + main1 = 3 rows
        assert_eq!(graph.rows.len(), 3);
        let (_, count) = graph.rows[1].collapsed.as_ref().unwrap();
        assert_eq!(*count, 2);
    }

    #[test]
    fn test_unlabeled_branch_collapsible() {
        let mut graph = GitGraph::new();
        // No labels on any side-branch row
        graph.set_rows(make_branch_rows(vec![]));
        graph.state.select(Some(1));
        graph.toggle_collapse_selected();

        assert!(!graph.collapsed_branches.is_empty());
        // Placeholder uses short OID as display name
        let placeholder = &graph.rows[1];
        assert!(placeholder.collapsed.is_some());
        assert!(placeholder.message.contains("a")); // short_id of tip
    }
}
