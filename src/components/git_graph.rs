use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::git::graph::{GraphBuilder, GraphRow};
use crate::git::graph_render;

pub(crate) struct GitGraph {
    rows: Vec<GraphRow>,
    state: ListState,
    repo_name: String,
    loading: bool,
    action_tx: Option<UnboundedSender<Action>>,
}

impl GitGraph {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            state: ListState::default(),
            repo_name: String::new(),
            loading: false,
            action_tx: None,
        }
    }

    pub fn load_repo(&mut self, path: PathBuf, repo_name: &str) {
        self.repo_name = repo_name.to_string();
        self.loading = true;
        self.rows.clear();
        self.state.select(None);

        let Some(tx) = &self.action_tx else { return };
        let tx = tx.clone();

        tokio::task::spawn_blocking(move || {
            let builder = GraphBuilder::new();
            match builder.build(&path) {
                Ok(rows) => {
                    let _ = tx.send(Action::GraphLoaded(rows));
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!("Failed to load graph: {}", e)));
                }
            }
        });
    }

    pub fn set_rows(&mut self, rows: Vec<GraphRow>) {
        self.rows = rows;
        self.loading = false;
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        }
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
}

impl Component for GitGraph {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

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
            KeyCode::Esc => Ok(Some(Action::ShowFileList)),
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let title = format!(" Git Graph — {} ", self.repo_name);

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        if self.loading {
            let paragraph = Paragraph::new("Loading...")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(paragraph, area);
            return Ok(());
        }

        if self.rows.is_empty() {
            let paragraph = Paragraph::new("No commits")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(paragraph, area);
            return Ok(());
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let mut spans = graph_render::render_graph_prefix(row);

                // Short hash
                spans.push(Span::styled(
                    format!("{} ", row.short_id),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));

                // Commit message
                spans.push(Span::styled(
                    &row.message,
                    Style::default().fg(Color::White),
                ));

                // Author (dimmed)
                spans.push(Span::styled(
                    format!("  — {}", row.author),
                    Style::default().fg(Color::DarkGray),
                ));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(list, area, &mut self.state);
        Ok(())
    }
}
