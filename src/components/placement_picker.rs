use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use std::sync::Arc;

use crate::action::Action;
use crate::theme::Theme;

/// Overlay shown when `[open]/[review] placement = "ask"`: pick where the new
/// tmux pane/window goes. Each choice is `(label, placement)`; on Enter it emits
/// the chosen placement string for the pending launch to resume with.
pub(crate) struct PlacementPicker {
    pub visible: bool,
    choices: Vec<(String, String)>,
    state: ListState,
    theme: Arc<Theme>,
}

impl PlacementPicker {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            choices: Vec::new(),
            state: ListState::default(),
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn show(&mut self, choices: Vec<(String, String)>) {
        self.choices = choices;
        self.state.select(Some(0));
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.choices.clear();
        self.state.select(None);
    }

    fn select_next(&mut self) {
        if self.choices.is_empty() {
            return;
        }
        let next = match self.state.selected() {
            Some(i) => (i + 1).min(self.choices.len() - 1),
            None => 0,
        };
        self.state.select(Some(next));
    }

    fn select_prev(&mut self) {
        let prev = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(prev));
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if !self.visible {
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
            KeyCode::Enter => {
                if let Some(i) = self.state.selected()
                    && let Some((_, placement)) = self.choices.get(i)
                {
                    return Ok(Some(Action::PlacementChosen(placement.clone())));
                }
                Ok(None)
            }
            KeyCode::Esc | KeyCode::Char('q') => Ok(Some(Action::CancelPlacement)),
            _ => Ok(None),
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let t = &self.theme.overlay;
        let inner_height = (self.choices.len() as u16).max(1);
        let height = (inner_height + 2).min(area.height.saturating_sub(4)).max(5);
        let width = 44u16.min(area.width.saturating_sub(4));

        let [vert] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vert);

        frame.render_widget(Clear, rect);

        let items: Vec<ListItem> = self
            .choices
            .iter()
            .map(|(label, _)| ListItem::new(Line::from(Span::raw(format!(" {label} ")))))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Open where? — ↑↓ select · Enter · Esc cancel ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.context_menu_border)),
            )
            .highlight_style(
                Style::default()
                    .bg(t.context_menu_selection_bg)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, rect, &mut self.state);
    }
}
