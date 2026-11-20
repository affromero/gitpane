use std::path::PathBuf;

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::*;
use crate::git::github::inbox::{self, Preview, PullRequest};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) enum Page {
    Inbox(bool, Result<Vec<PullRequest>, String>),
    Preview(Result<Preview, String>),
}

#[derive(Default)]
pub(super) struct Overlay {
    open: bool,
    generation: u64,
    reviews: bool,
    selected: usize,
    rows: Option<Result<Vec<PullRequest>, String>>,
    cached_reviews: Option<Vec<PullRequest>>,
    list_in_flight: bool,
    detail: bool,
    preview: Option<Result<Preview, String>>,
    target_index: Option<usize>,
    target_highlighted_at: Option<Instant>,
}

impl Overlay {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn review_count(&self) -> usize {
        self.cached_reviews.as_ref().map_or(0, Vec::len)
    }

    fn begin_list(&mut self) -> bool {
        !std::mem::replace(&mut self.list_in_flight, true)
    }

    fn finish_list(&mut self, reviews: bool, rows: Result<Vec<PullRequest>, String>) -> bool {
        self.list_in_flight = false;
        if reviews && let Ok(rows) = &rows {
            self.cached_reviews = Some(rows.clone());
        }
        if !self.open || self.detail || self.rows.is_some() {
            return false;
        }
        if self.reviews != reviews {
            return true;
        }
        self.rows = Some(rows);
        false
    }

    fn close(&mut self) {
        self.open = false;
        self.generation += 1;
    }

    fn complete_preview(&mut self, generation: u64, preview: Result<Preview, String>) {
        if generation != self.generation {
            return;
        }
        self.target_index = preview
            .as_ref()
            .ok()
            .filter(|preview| preview.targets.len() == 1)
            .map(|_| 0);
        self.target_highlighted_at = self.target_index.map(|_| Instant::now());
        self.preview = Some(preview);
    }

    fn target(&self) -> Option<&(PathBuf, Vec<String>)> {
        self.preview
            .as_ref()?
            .as_ref()
            .ok()?
            .targets
            .get(self.target_index?)
    }

    pub(super) fn highlighted_worktree(&self) -> Option<(PathBuf, bool)> {
        if !self.open || !self.detail {
            return None;
        }
        let (path, _) = self.target()?;
        let phase = self.target_highlighted_at?.elapsed().as_millis() / 350;
        Some((path.clone(), phase < 3 && phase.is_multiple_of(2)))
    }

    pub(super) fn worktree_pulse_active(&self) -> bool {
        self.open
            && self.detail
            && self
                .target_highlighted_at
                .is_some_and(|start| start.elapsed() < Duration::from_millis(1500))
    }

    fn choose_target(&mut self, forward: bool) {
        let Some(Ok(preview)) = &self.preview else {
            return;
        };
        let count = preview.targets.len();
        if count < 2 {
            return;
        }
        let index = match self.target_index {
            Some(index) if forward => (index + 1) % count,
            Some(index) => (index + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };
        self.target_index = Some(index);
        self.target_highlighted_at = Some(Instant::now());
    }
}

impl App {
    pub(super) fn load_initial_reviews(&mut self) {
        if self.config.github.enabled {
            self.start_inbox_list(true);
        }
    }

    pub(super) fn workflow_loaded(&mut self, generation: u64, page: Page) {
        match page {
            Page::Preview(preview) => self.workflow.complete_preview(generation, preview),
            Page::Inbox(reviews, rows) => {
                if self.workflow.finish_list(reviews, rows) {
                    self.load_inbox(false);
                }
            }
        }
    }

    pub(super) fn workflow_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        if !self.workflow.is_open() {
            match key.code {
                KeyCode::F(6) if self.config.github.enabled => {
                    self.workflow.open = true;
                    self.workflow.detail = false;
                    self.workflow.selected = 0;
                    self.load_inbox(false);
                }
                _ => return Ok(false),
            }
        } else if self.workflow.detail {
            match key.code {
                KeyCode::Esc => {
                    self.workflow.detail = false;
                    self.workflow.generation += 1;
                }
                KeyCode::Char('o') => {
                    if let Some((path, _)) = self.workflow.target() {
                        self.action_tx.send(Action::OpenAt(RepoId(path.clone())))?;
                    }
                }
                KeyCode::Char('G') => {
                    if let Some((path, sessions)) = self.workflow.target()
                        && !sessions.is_empty()
                    {
                        let target = RepoId(path.clone());
                        self.workflow.close();
                        self.action_tx.send(Action::GotoSessionPicker(target))?;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => self.workflow.choose_target(true),
                KeyCode::Up | KeyCode::Char('k') => self.workflow.choose_target(false),
                _ => {}
            }
        } else {
            let rows = self
                .workflow
                .rows
                .as_ref()
                .and_then(|rows| rows.as_ref().ok());
            match key.code {
                KeyCode::Esc => self.workflow.close(),
                KeyCode::Tab => {
                    self.workflow.reviews = !self.workflow.reviews;
                    self.workflow.selected = 0;
                    self.load_inbox(false);
                }
                KeyCode::Char('r') => {
                    self.workflow.selected = 0;
                    self.load_inbox(true);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(rows) = rows {
                        self.workflow.selected =
                            (self.workflow.selected + 1).min(rows.len().saturating_sub(1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.workflow.selected = self.workflow.selected.saturating_sub(1);
                }
                KeyCode::Enter => {
                    if let Some(row) = rows.and_then(|rows| rows.get(self.workflow.selected)) {
                        let repository = row.repository().to_owned();
                        let number = row.number;
                        let paths = self.workflow_paths();
                        self.workflow.detail = true;
                        self.workflow.preview = None;
                        self.workflow.target_index = None;
                        self.workflow.target_highlighted_at = None;
                        self.request_workflow(move || {
                            Page::Preview(
                                inbox::inspect(&paths, &repository, number)
                                    .map_err(|error| error.to_string()),
                            )
                        });
                    }
                }
                _ => {}
            }
        }
        self.action_tx.send(Action::Render)?;
        Ok(true)
    }

    fn workflow_paths(&self) -> Vec<PathBuf> {
        self.repo_list
            .repos
            .iter()
            .map(|repo| repo.path.clone())
            .collect()
    }

    fn load_inbox(&mut self, refresh: bool) {
        let reviews = self.workflow.reviews;
        if reviews
            && !refresh
            && let Some(rows) = &self.workflow.cached_reviews
        {
            self.workflow.generation += 1;
            self.workflow.rows = Some(Ok(rows.clone()));
            return;
        }
        self.workflow.rows = None;
        self.start_inbox_list(reviews);
    }

    fn start_inbox_list(&mut self, reviews: bool) {
        if !self.workflow.begin_list() {
            return;
        }
        let paths = self.workflow_paths();
        self.request_workflow(move || Page::Inbox(reviews, inbox::list(&paths, reviews)));
    }

    fn request_workflow(&mut self, load: impl FnOnce() -> Page + Send + 'static) {
        self.workflow.generation += 1;
        let generation = self.workflow.generation;
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Action::WorkflowLoaded(generation, load()));
        });
    }

    pub(super) fn draw_workflow(&self, frame: &mut ratatui::Frame, area: Rect) {
        let modal = &self.workflow;
        let theme = &self.theme.overlay;
        let width = area.width.min(86);
        let height = area.height.min(18);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let mut lines = Vec::new();
        let title;
        let footer;
        if modal.detail {
            title = " Pull request ";
            footer = match &modal.preview {
                Some(Ok(preview)) if preview.targets.len() == 1 => {
                    " o Open worktree   G Attach session   Esc Back "
                }
                Some(Ok(preview)) if preview.targets.len() > 1 => {
                    " ↑↓ Choose worktree   o Open   G Attach   Esc Back "
                }
                _ => " Esc Back ",
            };
            match &modal.preview {
                None => lines.push(Line::raw("Loading preview…")),
                Some(Err(error)) => lines.push(Line::raw(safe_text(error))),
                Some(Ok(preview)) => {
                    lines.push(Line::styled(
                        safe_text(&preview.title),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    lines.extend(
                        preview
                            .body
                            .lines()
                            .take(3)
                            .map(|line| Line::raw(safe_text(line))),
                    );
                    lines.push(Line::raw(""));
                    if preview.targets.is_empty() {
                        lines.push(Line::raw("No linked worktree tracks the PR head"));
                    }
                    let visible = height.saturating_sub(lines.len() as u16 + 2).max(1) as usize;
                    let start = (modal.target_index.unwrap_or(0) + 1).saturating_sub(visible);
                    for (index, (path, sessions)) in
                        preview.targets.iter().enumerate().skip(start).take(visible)
                    {
                        let selected = modal.target_index == Some(index);
                        let marker = if selected { "› " } else { "  " };
                        let name = if preview.targets.len() == 1 {
                            path.file_name().unwrap_or_default().to_string_lossy()
                        } else {
                            path.to_string_lossy()
                        };
                        let style = if selected {
                            Style::default()
                                .fg(theme.help_key)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        lines.push(Line::styled(
                            format!("{marker}Worktree  {}", safe_text(&name)),
                            style,
                        ));
                        if preview.targets.len() > 1 {
                            continue;
                        }
                        for session in sessions {
                            lines.push(Line::raw(format!("tmux      {}", safe_text(session))));
                        }
                    }
                }
            }
        } else {
            title = if modal.reviews {
                " PR inbox · Review requests "
            } else {
                " PR inbox · My PRs "
            };
            footer = " Tab Switch list   ↑↓ Select   Enter Preview   r Refresh   Esc Close ";
            match &modal.rows {
                None => lines.push(Line::raw("Loading pull requests…")),
                Some(Err(error)) => lines.push(Line::raw(safe_text(error))),
                Some(Ok(rows)) if rows.is_empty() => lines.push(Line::raw("No open pull requests")),
                Some(Ok(rows)) => {
                    let visible = height.saturating_sub(3).max(1) as usize;
                    let start = (modal.selected + 1).saturating_sub(visible);
                    for (index, row) in rows.iter().enumerate().skip(start).take(visible) {
                        let selected = index == modal.selected;
                        let marker = if selected { "›" } else { " " };
                        let draft = if row.is_draft { " [draft]" } else { "" };
                        let local = if row.local_count > 0 { " · local" } else { "" };
                        let style = if selected {
                            Style::default()
                                .fg(theme.help_key)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        lines.push(Line::styled(
                            format!(
                                "{marker} {} #{}{draft}  {}{local}",
                                safe_text(row.repository()),
                                row.number,
                                safe_text(&row.title)
                            ),
                            style,
                        ));
                    }
                }
            }
        }
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(footer)
            .border_style(Style::default().fg(theme.help_border))
            .style(Style::default().bg(theme.help_bg));
        let paragraph = Paragraph::new(lines).block(block);
        if modal.detail {
            frame.render_widget(paragraph.wrap(Wrap { trim: true }), rect);
        } else {
            frame.render_widget(paragraph, rect);
        }
    }
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(280)
        .collect()
}
