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
use crate::git::graph::{GraphBuilder, GraphOptions, GraphRow};
use crate::git::graph_render;

/// Given a graph row, determine if `click_col` (relative to the start of the row content)
/// falls on a branch label. Returns the label name if so.
fn label_at_column(row: &GraphRow, click_col: usize, label_max_len: usize) -> Option<String> {
    if row.labels.is_empty() {
        return None;
    }

    // Compute graph prefix width
    let prefix_width: usize = graph_render::render_graph_prefix(row)
        .iter()
        .map(|s| s.content.chars().count())
        .sum();

    // short_id + space
    let id_width = row.short_id.len() + 1;

    let mut cursor = prefix_width + id_width;

    // Opening paren "("
    cursor += 1;

    for (i, label) in row.labels.iter().enumerate() {
        if i > 0 {
            cursor += 2; // ", "
        }

        // Prefix for HEAD or worktree labels
        let prefix_len = if label.is_head || label.is_worktree {
            2
        } else {
            0
        };
        cursor += prefix_len;

        // Label name (may be truncated)
        let name_len = if label.name.chars().count() > label_max_len {
            label_max_len + 1 // truncated chars + ellipsis
        } else {
            label.name.chars().count()
        };

        let label_start = cursor;
        cursor += name_len;

        if click_col >= label_start && click_col < cursor {
            return Some(label.name.clone());
        }
    }

    None
}

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
    rows: Vec<GraphRow>,
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
    show_help: bool,
}

impl GitGraph {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
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
            show_help: false,
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
            self.state.select(None);
            self.commit_detail = None;
            self.search.clear();
            self.graph_options.hidden_branches.clear();
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
        // Carry forward diff_stats from previous rows to avoid blink on refresh
        if !self.rows.is_empty() {
            let old_stats: std::collections::HashMap<git2::Oid, crate::git::graph::DiffStat> = self
                .rows
                .iter()
                .filter_map(|r| r.diff_stat.clone().map(|s| (r.oid, s)))
                .collect();
            for row in &mut rows {
                if row.diff_stat.is_none() {
                    row.diff_stat = old_stats.get(&row.oid).cloned();
                }
            }
        }
        self.rows = rows;
        self.loading = false;
        if !self.rows.is_empty() {
            let idx = prev_selected
                .map(|i| i.min(self.rows.len() - 1))
                .unwrap_or(0);
            self.state.select(Some(idx));
        }
    }

    pub fn set_diff_stats(&mut self, stats: Vec<(git2::Oid, crate::git::graph::DiffStat)>) {
        let stat_map: std::collections::HashMap<_, _> = stats.into_iter().collect();
        for row in &mut self.rows {
            if let Some(stat) = stat_map.get(&row.oid) {
                row.diff_stat = Some(stat.clone());
            }
        }
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

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Hide a branch and reload the graph.
    fn toggle_branch(&mut self, name: String) {
        if self.graph_options.hidden_branches.contains(&name) {
            self.graph_options.hidden_branches.remove(&name);
        } else {
            self.graph_options.hidden_branches.insert(name);
        }
        self.reload_graph();
    }

    /// Show all hidden branches and reload the graph.
    fn show_all_branches(&mut self) {
        if self.graph_options.hidden_branches.is_empty() {
            return;
        }
        self.graph_options.hidden_branches.clear();
        self.reload_graph();
    }

    fn reload_graph(&mut self) {
        if let Some(path) = self.repo_path.clone() {
            let name = self.repo_name.clone();
            self.load_repo(path, &name);
        }
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
        let row = self.rows.get(idx)?;
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
        self.search.matches.clear();
        self.search.current_match = None;
        if self.search.input.is_empty() {
            return;
        }
        let query = self.search.input.to_lowercase();
        for (i, row) in self.rows.iter().enumerate() {
            if row.message.to_lowercase().contains(&query)
                || row.author.to_lowercase().contains(&query)
                || row.short_id.to_lowercase().contains(&query)
            {
                self.search.matches.push(i);
            }
        }
        if !self.search.matches.is_empty() {
            self.search.current_match = Some(0);
        }
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
        if self.rows.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.rows.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() {
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
        let row = self.rows.get(idx)?;
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
        let hidden_count = self.graph_options.hidden_branches.len();
        let title = match (self.graph_options.first_parent, hidden_count) {
            (true, 0) => format!(" Git Graph — {} [1st-parent] ", self.repo_name),
            (true, n) => format!(" Git Graph — {} [1st-parent] ({n} hidden) ", self.repo_name),
            (false, 0) => format!(" Git Graph — {} ", self.repo_name),
            (false, n) => format!(" Git Graph — {} ({n} hidden) ", self.repo_name),
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

        if self.rows.is_empty() {
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
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let dimmed = has_search && !self.search.matches.contains(&i);
                let mut spans = graph_render::render_graph_prefix(row);

                if dimmed {
                    // Override all spans to DarkGray
                    for span in &mut spans {
                        span.style = Style::default().fg(Color::DarkGray);
                    }
                }

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

                graph_render::truncate_line(&mut spans, max_width);
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
        // Global keys that work in any state
        match key.code {
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                return Ok(None);
            }
            _ => {
                if self.show_help {
                    self.show_help = false;
                    return Ok(None);
                }
            }
        }

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
            KeyCode::Char('H') => {
                self.show_all_branches();
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
                    let content_x = self.graph_list_area.x + 1;
                    if mouse.row >= content_y {
                        let visual_row = (mouse.row - content_y) as usize;
                        let idx = visual_row + self.state.offset();
                        if idx < self.rows.len() {
                            // Check if click landed on a branch label
                            let click_col = (mouse.column.saturating_sub(content_x)) as usize;
                            if let Some(branch_name) = label_at_column(
                                &self.rows[idx],
                                click_col,
                                self.graph_options.label_max_len,
                            ) {
                                self.toggle_branch(branch_name);
                                return Ok(None);
                            }

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
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;

        match &self.commit_detail {
            Some(detail) if detail.diff_content.is_some() => {
                // Graph 40% | Files 25% | Diff 35%
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
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
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
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

        // Help overlay
        if self.show_help {
            self.draw_help(frame, area);
        }

        Ok(())
    }
}

impl GitGraph {
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let help_lines = vec![
            Line::from(vec![
                Span::styled("  ?", Style::default().fg(Color::Yellow)),
                Span::raw("          Toggle this help"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " Navigation",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("  j/k", Style::default().fg(Color::Yellow)),
                Span::raw("        Move up/down"),
            ]),
            Line::from(vec![
                Span::styled("  Enter", Style::default().fg(Color::Yellow)),
                Span::raw("      Open commit files"),
            ]),
            Line::from(vec![
                Span::styled("  Esc", Style::default().fg(Color::Yellow)),
                Span::raw("        Close panel / go back"),
            ]),
            Line::from(vec![
                Span::styled("  Tab", Style::default().fg(Color::Yellow)),
                Span::raw("        Cycle focus"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " Search",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("  /", Style::default().fg(Color::Yellow)),
                Span::raw("          Search commits"),
            ]),
            Line::from(vec![
                Span::styled("  n / N", Style::default().fg(Color::Yellow)),
                Span::raw("      Next / prev match"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " View",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("  f", Style::default().fg(Color::Yellow)),
                Span::raw("          Toggle first-parent mode"),
            ]),
            Line::from(vec![
                Span::styled("  click", Style::default().fg(Color::Yellow)),
                Span::raw("      Click branch label to hide"),
            ]),
            Line::from(vec![
                Span::styled("  H", Style::default().fg(Color::Yellow)),
                Span::raw("          Show all hidden branches"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                " Other",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("  y", Style::default().fg(Color::Yellow)),
                Span::raw("          Copy to clipboard"),
            ]),
            Line::from(vec![
                Span::styled("  r", Style::default().fg(Color::Yellow)),
                Span::raw("          Refresh"),
            ]),
            Line::from(vec![
                Span::styled("  q", Style::default().fg(Color::Yellow)),
                Span::raw("          Quit"),
            ]),
        ];

        let height = (help_lines.len() as u16 + 2).min(area.height);
        let width = 40u16.min(area.width);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let help_area = Rect::new(x, y, width, height);

        let block = Block::default()
            .title(" Keybindings ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Black));

        // Clear the area behind the overlay
        frame.render_widget(ratatui::widgets::Clear, help_area);
        let paragraph = Paragraph::new(help_lines).block(block);
        frame.render_widget(paragraph, help_area);
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
            diff_stat: None,
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

    fn mock_row_with_labels(labels: Vec<BranchLabel>) -> GraphRow {
        let mut row = mock_row("abc1234", "commit msg", "Author");
        row.labels = labels;
        row
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

    #[test]
    fn test_label_click_detects_branch_name() {
        let row = mock_row_with_labels(vec![make_label("main")]);
        // Layout: "● " (3 chars: commit + space + space) + "abc1234 " (8) + "(" (1) + "main" (4)
        // Graph prefix for single Commit lane = "●" (1 char) but rendered with separator
        // render_graph_prefix returns spans: ["●"] = 1 char
        // So cursor: prefix(1) + id(8) + paren(1) = 10, label "main" at cols 10..14
        let prefix_width: usize = graph_render::render_graph_prefix(&row)
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        let label_start = prefix_width + 8 + 1; // prefix + "abc1234 " + "("
        assert_eq!(
            label_at_column(&row, label_start, 24),
            Some("main".to_string())
        );
        assert_eq!(
            label_at_column(&row, label_start + 3, 24),
            Some("main".to_string())
        );
        // One past the end of "main" should not match
        assert_eq!(label_at_column(&row, label_start + 4, 24), None);
    }

    #[test]
    fn test_label_click_returns_none_for_no_labels() {
        let row = mock_row("abc1234", "commit msg", "Author");
        assert_eq!(label_at_column(&row, 0, 24), None);
        assert_eq!(label_at_column(&row, 15, 24), None);
    }

    #[test]
    fn test_label_click_multiple_labels() {
        let row = mock_row_with_labels(vec![make_label("main"), make_label("dev")]);
        let prefix_width: usize = graph_render::render_graph_prefix(&row)
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        let main_start = prefix_width + 8 + 1; // after "("
        let dev_start = main_start + 4 + 2; // "main" + ", "
        assert_eq!(
            label_at_column(&row, main_start, 24),
            Some("main".to_string())
        );
        assert_eq!(
            label_at_column(&row, dev_start, 24),
            Some("dev".to_string())
        );
    }

    #[test]
    fn test_label_click_head_label_has_prefix() {
        let mut label = make_label("main");
        label.is_head = true;
        let row = mock_row_with_labels(vec![label]);
        let prefix_width: usize = graph_render::render_graph_prefix(&row)
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        // After "(" + "* " (2 char prefix), then "main"
        let label_start = prefix_width + 8 + 1 + 2;
        assert_eq!(
            label_at_column(&row, label_start, 24),
            Some("main".to_string())
        );
    }
}
