use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::sync::Arc;

use crate::action::Action;
use crate::components::Component;
use crate::git::github::{GhItem, ItemDetail};
use crate::theme::Theme;

/// One selectable row: either an issue or a pull request.
struct PanelRow {
    is_pr: bool,
    item: GhItem,
}

/// What the panel shows when it has no rows to list.
enum PanelStatus {
    /// Rows are authoritative (may still be empty = "no open items").
    Ready,
    /// A fetch is in flight and there is nothing cached yet.
    Loading,
    /// The selected repo has no github.com `origin`.
    NotGithub,
    /// A `gh` invocation failed; the message is its first stderr line.
    Error(String),
}

/// The 4th panel: open issues and pull requests for the selected repo, fetched
/// through the `gh` CLI. It is a pure view — [`crate::app::App`] owns the cache
/// and feeds this component on selection change and when a fetch completes.
pub(crate) struct GithubPanel {
    rows: Vec<PanelRow>,
    issue_count: usize,
    pr_count: usize,
    state: ListState,
    repo_name: String,
    status: PanelStatus,
    pub focused: bool,
    /// Fed by `App` each frame: split direction follows the main layout.
    pub horizontal_layout: bool,
    render_area: Rect,
    list_area: Rect,
    detail_area: Rect,
    /// Loaded body + comments for the selected item; `Some` opens the detail
    /// pane. `None` with `detail_loading`/`detail_error` covers the in-flight
    /// and failed states.
    detail: Option<ItemDetail>,
    detail_loading: bool,
    detail_error: Option<String>,
    detail_scroll: u16,
    /// Bumped per detail request so a late `gh view` result is discarded.
    detail_generation: u64,
    theme: Arc<Theme>,
}

impl GithubPanel {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            rows: Vec::new(),
            issue_count: 0,
            pr_count: 0,
            state: ListState::default(),
            repo_name: String::new(),
            status: PanelStatus::Loading,
            focused: false,
            horizontal_layout: false,
            render_area: Rect::default(),
            list_area: Rect::default(),
            detail_area: Rect::default(),
            detail: None,
            detail_loading: false,
            detail_error: None,
            detail_scroll: 0,
            detail_generation: 0,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    /// Replace the panel's data. Selection is preserved when the repo is
    /// unchanged (a refetch of the same repo), reset to the top otherwise.
    pub fn set_data(&mut self, issues: Vec<GhItem>, prs: Vec<GhItem>, repo_name: &str) {
        let same_repo = self.repo_name == repo_name;
        let prev = self.state.selected();
        self.issue_count = issues.len();
        self.pr_count = prs.len();
        self.rows = issues
            .into_iter()
            .map(|item| PanelRow { is_pr: false, item })
            .chain(prs.into_iter().map(|item| PanelRow { is_pr: true, item }))
            .collect();
        self.repo_name = repo_name.to_string();
        self.status = PanelStatus::Ready;

        if self.rows.is_empty() {
            self.state.select(None);
        } else if same_repo {
            let idx = prev.map(|i| i.min(self.rows.len() - 1)).unwrap_or(0);
            self.state.select(Some(idx));
        } else {
            self.state.select(Some(0));
        }
    }

    pub fn set_loading(&mut self, repo_name: &str) {
        self.reset_if_new(repo_name);
        self.status = PanelStatus::Loading;
    }

    pub fn set_not_github(&mut self, repo_name: &str) {
        self.reset_if_new(repo_name);
        self.status = PanelStatus::NotGithub;
    }

    pub fn set_error(&mut self, repo_name: &str, message: String) {
        self.reset_if_new(repo_name);
        self.status = PanelStatus::Error(message);
    }

    /// Drop stale rows when the panel retargets a different repo. On a same-repo
    /// status change (e.g. a refetch) the rows stay put so the list doesn't
    /// flicker while new data loads.
    fn reset_if_new(&mut self, repo_name: &str) {
        if self.repo_name != repo_name {
            self.rows.clear();
            self.issue_count = 0;
            self.pr_count = 0;
            self.state.select(None);
            self.close_detail();
        }
        self.repo_name = repo_name.to_string();
    }

    /// Web URL of the selected row, for opening in the browser.
    pub fn selected_url(&self) -> Option<String> {
        let idx = self.state.selected()?;
        self.rows.get(idx).map(|r| r.item.url.clone())
    }

    /// Whether the detail (body + comments) pane is open — loading, loaded, or
    /// showing a fetch error.
    pub fn has_detail(&self) -> bool {
        self.detail_loading || self.detail.is_some() || self.detail_error.is_some()
    }

    /// Close the detail pane and reset its scroll.
    pub fn close_detail(&mut self) {
        self.detail = None;
        self.detail_loading = false;
        self.detail_error = None;
        self.detail_scroll = 0;
    }

    /// Apply a detail fetch result, discarding it if a newer request superseded
    /// this one (generation mismatch).
    pub fn set_detail(&mut self, generation: u64, result: Result<ItemDetail, String>) {
        if generation != self.detail_generation {
            return;
        }
        self.detail_loading = false;
        match result {
            Ok(detail) => {
                self.detail = Some(detail);
                self.detail_error = None;
            }
            Err(e) => {
                self.detail = None;
                self.detail_error = Some(e);
            }
        }
    }

    /// Begin loading the selected item's detail: bump the generation, mark the
    /// pane loading, and return the fetch action for `App` to run.
    fn open_selected_detail(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        let row = self.rows.get(idx)?;
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.detail_loading = true;
        self.detail = None;
        self.detail_error = None;
        self.detail_scroll = 0;
        Some(Action::ShowGithubItem {
            url: row.item.url.clone(),
            is_pr: row.is_pr,
            generation: self.detail_generation,
        })
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

    fn row_line(&self, row: &PanelRow) -> Line<'static> {
        let f = &self.theme.file_list;
        let g = &self.theme.graph;
        let r = &self.theme.repo_list;

        let (tag, tag_color) = if row.is_pr {
            if row.item.is_draft {
                ("PR draft", f.empty_text)
            } else {
                ("PR", r.ahead)
            }
        } else {
            ("issue", f.status_modified)
        };

        let date = row
            .item
            .updated_at
            .get(..10)
            .unwrap_or(&row.item.updated_at)
            .to_string();

        Line::from(vec![
            Span::styled(
                format!("#{} ", row.item.number),
                Style::default()
                    .fg(g.commit_id)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{tag} "), Style::default().fg(tag_color)),
            Span::styled(row.item.title.clone(), Style::default().fg(f.regular_path)),
            Span::styled(
                format!("  {} \u{00b7} {}", row.item.author, date),
                Style::default().fg(g.time),
            ),
        ])
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        self.list_area = area;
        let f = &self.theme.file_list;
        let border_color = if self.focused && !self.has_detail() {
            f.border_focused
        } else {
            f.border_unfocused
        };

        let title = if self.repo_name.is_empty() {
            " GitHub ".to_string()
        } else {
            format!(
                " GitHub \u{2014} {} ({} issues, {} PRs) ",
                self.repo_name, self.issue_count, self.pr_count
            )
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        if self.rows.is_empty() {
            let msg = match &self.status {
                PanelStatus::Loading => "Loading\u{2026}",
                PanelStatus::NotGithub => "No github.com remote",
                PanelStatus::Error(e) => e.as_str(),
                PanelStatus::Ready => "No open issues or PRs",
            };
            let paragraph = Paragraph::new(msg)
                .style(Style::default().fg(f.empty_text))
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| ListItem::new(self.row_line(row)))
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(f.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        self.detail_area = area;
        let f = &self.theme.file_list;
        let g = &self.theme.graph;

        let title = match &self.detail {
            Some(d) => format!(" #{} \u{2014} {} (Esc to close) ", d.number, d.title),
            None => " GitHub item (Esc to close) ".to_string(),
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(f.diff_border));

        let lines: Vec<Line> = if self.detail_loading {
            vec![Line::from(Span::styled(
                "Loading\u{2026}",
                Style::default().fg(f.empty_text),
            ))]
        } else if let Some(e) = &self.detail_error {
            vec![Line::from(Span::styled(
                e.clone(),
                Style::default().fg(g.error_text),
            ))]
        } else if let Some(d) = &self.detail {
            self.detail_lines(d)
        } else {
            Vec::new()
        };

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(paragraph, area);
    }

    /// Body + comment thread of an item as styled, wrappable lines.
    fn detail_lines(&self, d: &ItemDetail) -> Vec<Line<'static>> {
        let f = &self.theme.file_list;
        let g = &self.theme.graph;
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                format!("@{}", d.author),
                Style::default().fg(g.time),
            )),
            Line::from(""),
        ];
        for line in d.body.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(f.regular_path),
            )));
        }
        for c in &d.comments {
            lines.push(Line::from(""));
            let date = c.created_at.get(..10).unwrap_or(&c.created_at);
            lines.push(Line::from(Span::styled(
                format!("\u{2500}\u{2500} @{} \u{00b7} {}", c.author, date),
                Style::default().fg(g.commit_id),
            )));
            for line in c.body.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(f.regular_path),
                )));
            }
        }
        lines
    }
}

impl Component for GithubPanel {
    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        // Detail pane open: keys scroll or close it, or open the item in the
        // browser. ghpeek-style: Enter in the list previews, Enter again opens.
        if self.has_detail() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                    self.close_detail();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                }
                KeyCode::Enter => return Ok(self.selected_url().map(Action::OpenUrl)),
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.open_selected_detail()),
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                // When split, only the list half selects rows.
                let click_area = if self.has_detail() {
                    self.list_area
                } else {
                    self.render_area
                };
                if click_area.contains(pos) {
                    let content_y = click_area.y + 1; // +1 for border
                    if mouse.row >= content_y {
                        let idx = (mouse.row - content_y) as usize + self.state.offset();
                        if idx < self.rows.len() {
                            // Click the already-selected row to open its detail.
                            if self.state.selected() == Some(idx) {
                                return Ok(self.open_selected_detail());
                            }
                            self.state.select(Some(idx));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                if self.has_detail() {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else {
                    self.select_prev();
                }
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                if self.has_detail() {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
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
        if self.has_detail() {
            // A narrow column (horizontal main layout) stacks list over detail;
            // a wide short row splits them side by side. Mirrors the changes
            // panel's diff split.
            let dir = if self.horizontal_layout {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(area);
            self.draw_list(frame, chunks[0]);
            self.draw_detail(frame, chunks[1]);
        } else {
            self.detail_area = Rect::default();
            self.draw_list(frame, area);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: u64) -> GhItem {
        GhItem {
            number: n,
            title: format!("title {n}"),
            is_draft: false,
            author: "octocat".into(),
            updated_at: "2026-01-02T03:04:05Z".into(),
            url: format!("https://github.com/o/r/issues/{n}"),
        }
    }

    fn panel() -> GithubPanel {
        GithubPanel::new(Arc::new(Theme::default()))
    }

    #[test]
    fn set_data_orders_issues_before_prs_and_counts_each() {
        let mut p = panel();
        p.set_data(vec![item(1), item(2)], vec![item(3)], "repo");
        assert_eq!(p.issue_count, 2);
        assert_eq!(p.pr_count, 1);
        assert_eq!(p.rows.len(), 3);
        assert!(!p.rows[0].is_pr);
        assert!(!p.rows[1].is_pr);
        assert!(p.rows[2].is_pr);
        // Selection lands on the first row; its URL is what Enter would open.
        assert_eq!(
            p.selected_url().as_deref(),
            Some("https://github.com/o/r/issues/1")
        );
    }

    #[test]
    fn selection_persists_on_same_repo_and_resets_on_a_new_one() {
        let mut p = panel();
        p.set_data(vec![item(1), item(2), item(3)], vec![], "repo");
        p.state.select(Some(2));
        p.set_data(vec![item(1), item(2), item(3)], vec![], "repo");
        assert_eq!(p.state.selected(), Some(2), "same repo keeps selection");
        p.set_data(vec![item(9)], vec![], "other");
        assert_eq!(p.state.selected(), Some(0), "new repo resets selection");
    }

    #[test]
    fn empty_result_clears_selection_and_url() {
        let mut p = panel();
        p.set_data(vec![], vec![], "repo");
        assert_eq!(p.state.selected(), None);
        assert!(p.selected_url().is_none());
    }

    #[test]
    fn retargeting_to_a_new_repo_drops_stale_rows() {
        let mut p = panel();
        p.set_data(vec![item(1)], vec![item(2)], "repo");
        p.set_loading("other");
        assert!(p.rows.is_empty(), "loading a new repo clears old rows");
        assert_eq!(p.issue_count, 0);
        assert_eq!(p.pr_count, 0);
    }

    fn detail(n: u64) -> ItemDetail {
        ItemDetail {
            number: n,
            title: format!("title {n}"),
            author: "alice".into(),
            body: "body".into(),
            comments: vec![],
        }
    }

    #[test]
    fn detail_opens_loads_and_discards_stale_results() {
        let mut p = panel();
        p.set_data(vec![item(1)], vec![], "repo");
        let generation = match p.open_selected_detail().expect("enter yields an action") {
            Action::ShowGithubItem {
                generation, is_pr, ..
            } => {
                assert!(!is_pr, "issue, not PR");
                generation
            }
            other => panic!("expected ShowGithubItem, got {other:?}"),
        };
        assert!(p.has_detail(), "pane is open (loading) once requested");

        // A result from a superseded request is dropped.
        p.set_detail(generation.wrapping_sub(1), Ok(detail(99)));
        assert!(p.detail.is_none(), "stale generation ignored");

        // The current-generation result lands.
        p.set_detail(generation, Ok(detail(1)));
        assert_eq!(p.detail.as_ref().unwrap().number, 1);

        p.close_detail();
        assert!(!p.has_detail(), "closing clears the pane");
    }
}
