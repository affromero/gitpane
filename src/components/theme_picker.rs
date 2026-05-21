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
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::theme::Theme;

pub(crate) struct ThemePicker {
    pub visible: bool,
    themes: Vec<String>,
    state: ListState,
    original_name: Option<String>,
    /// Theme snapshot captured when the picker opened. Used by cancel so we
    /// restore the actual colors that were live, even if `original_name`
    /// no longer resolves (e.g. the custom file was deleted while the
    /// picker was open).
    original_theme: Option<Arc<Theme>>,
    action_tx: Option<UnboundedSender<Action>>,
    theme: Arc<Theme>,
}

impl ThemePicker {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            themes: Vec::new(),
            state: ListState::default(),
            original_name: None,
            original_theme: None,
            action_tx: None,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn register_action_handler(&mut self, tx: UnboundedSender<Action>) {
        self.action_tx = Some(tx);
    }

    /// Open the picker with the given list of theme names, centering selection
    /// on `current` if present. The currently-active `Arc<Theme>` is captured
    /// so cancel can restore it byte-for-byte.
    pub fn show(&mut self, themes: Vec<String>, current: &str, current_theme: Arc<Theme>) {
        self.themes = themes;
        self.original_name = Some(current.to_string());
        self.original_theme = Some(current_theme);
        let initial = self.themes.iter().position(|n| n == current).unwrap_or(0);
        self.state.select(Some(initial));
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.themes.clear();
        self.original_name = None;
        self.original_theme = None;
        self.state.select(None);
    }

    pub fn original_name(&self) -> Option<String> {
        self.original_name.clone()
    }

    pub fn original_theme(&self) -> Option<Arc<Theme>> {
        self.original_theme.clone()
    }

    fn select_next(&mut self) -> Option<&String> {
        if self.themes.is_empty() {
            return None;
        }
        let next = match self.state.selected() {
            Some(i) => (i + 1).min(self.themes.len() - 1),
            None => 0,
        };
        self.state.select(Some(next));
        self.themes.get(next)
    }

    fn select_prev(&mut self) -> Option<&String> {
        if self.themes.is_empty() {
            return None;
        }
        let prev = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(prev));
        self.themes.get(prev)
    }

    fn emit(&self, action: Action) {
        if let Some(tx) = &self.action_tx {
            let _ = tx.send(action);
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(name) = self.select_next().cloned() {
                    self.emit(Action::PreviewTheme(name));
                }
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(name) = self.select_prev().cloned() {
                    self.emit(Action::PreviewTheme(name));
                }
                Ok(None)
            }
            KeyCode::Enter => {
                if let Some(i) = self.state.selected()
                    && let Some(name) = self.themes.get(i).cloned()
                {
                    return Ok(Some(Action::CommitTheme(name)));
                }
                Ok(None)
            }
            KeyCode::Esc | KeyCode::Char('q') => Ok(Some(Action::CancelThemePreview)),
            _ => Ok(None),
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let t = &self.theme.overlay;
        let inner_height = (self.themes.len() as u16).max(1);
        let height = (inner_height + 2).min(area.height.saturating_sub(4)).max(5);
        let width = 36u16.min(area.width.saturating_sub(4));

        let [vert] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vert);

        frame.render_widget(Clear, rect);

        let original = self.original_name.as_deref();
        let items: Vec<ListItem> = self
            .themes
            .iter()
            .map(|name| {
                let mark = if original == Some(name.as_str()) {
                    "*"
                } else {
                    " "
                };
                let style = Style::default().fg(t.context_menu_push);
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {mark} "), style),
                    Span::raw(name.clone()),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Themes — ↑↓ preview · Enter commit · Esc cancel ")
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
