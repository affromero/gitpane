use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::sync::Arc;

use crate::action::Action;
use crate::components::Component;
use crate::git::github::GhItem;
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
    render_area: Rect,
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
            render_area: Rect::default(),
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
        }
        self.repo_name = repo_name.to_string();
    }

    /// Web URL of the selected row, for opening in the browser.
    pub fn selected_url(&self) -> Option<String> {
        let idx = self.state.selected()?;
        self.rows.get(idx).map(|r| r.item.url.clone())
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
}

impl Component for GithubPanel {
    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.selected_url().map(Action::OpenUrl)),
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                if self.render_area.contains(pos) {
                    let content_y = self.render_area.y + 1; // +1 for border
                    if mouse.row >= content_y {
                        let idx = (mouse.row - content_y) as usize + self.state.offset();
                        if idx < self.rows.len() {
                            if self.state.selected() == Some(idx) {
                                return Ok(self
                                    .rows
                                    .get(idx)
                                    .map(|r| Action::OpenUrl(r.item.url.clone())));
                            }
                            self.state.select(Some(idx));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                self.select_prev();
                Ok(None)
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;
        let f = &self.theme.file_list;
        let border_color = if self.focused {
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
            return Ok(());
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
}
