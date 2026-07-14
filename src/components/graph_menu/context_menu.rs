use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::action::Action;
use crate::theme::Theme;

/// Right-click menu for a selected graph commit.
pub(crate) struct GraphContextMenu {
    pub visible: bool,
    items: Vec<(String, Action)>,
    state: ListState,
    rendered_area: Rect,
    theme: Arc<Theme>,
}

impl GraphContextMenu {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            state: ListState::default(),
            rendered_area: Rect::default(),
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn show(
        &mut self,
        full_hash: String,
        short_hash: String,
        message: String,
        first_parent: bool,
        can_collapse: bool,
    ) {
        self.items = vec![
            (
                "Open commit files".to_string(),
                Action::OpenGraphCommitFiles,
            ),
            (
                "Copy full commit hash".to_string(),
                Action::CopyText(full_hash),
            ),
            (
                "Copy short commit hash".to_string(),
                Action::CopyText(short_hash),
            ),
            ("Copy commit subject".to_string(), Action::CopyText(message)),
            ("Search commits".to_string(), Action::OpenGraphSearch),
            ("Filter graph…".to_string(), Action::OpenGraphFilters),
            (
                if first_parent {
                    "Disable first-parent view".to_string()
                } else {
                    "Enable first-parent view".to_string()
                },
                Action::SetGraphFirstParent(!first_parent),
            ),
        ];
        if can_collapse {
            self.items.push((
                "Collapse / expand branch".to_string(),
                Action::ToggleGraphCollapse,
            ));
        }
        self.items.push((
            "Expand all branches".to_string(),
            Action::ExpandAllGraphBranches,
        ));
        self.state.select(Some(0));
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.state.select(None);
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(index) = self.state.selected() {
                    self.state
                        .select(Some((index + 1).min(self.items.len().saturating_sub(1))));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.state
                    .select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)));
            }
            KeyCode::Enter => {
                if let Some(index) = self.state.selected()
                    && let Some((_, action)) = self.items.get(index).cloned()
                {
                    self.hide();
                    return Ok(Some(action));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.hide(),
            _ => {}
        }
        Ok(None)
    }

    pub fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        if !self.visible || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return Ok(None);
        }
        let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
        if !self.rendered_area.contains(pos) {
            self.hide();
            return Ok(None);
        }
        let index = mouse.row.saturating_sub(self.rendered_area.y + 1) as usize;
        if let Some((_, action)) = self.items.get(index).cloned() {
            self.hide();
            return Ok(Some(action));
        }
        Ok(None)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }
        let height = (self.items.len() as u16 + 2)
            .min(area.height.saturating_sub(4))
            .max(5);
        let width = 34u16.min(area.width.saturating_sub(4));
        let [vertical] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vertical);
        self.rendered_area = rect;
        let t = &self.theme.overlay;
        frame.render_widget(Clear, rect);
        let items = self
            .items
            .iter()
            .map(|(label, _)| ListItem::new(format!(" {label} ")));
        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Commit actions — ↑↓ select · Enter · Esc close ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn clicking_copy_full_hash_emits_its_copy_action() {
        let mut menu = GraphContextMenu::new(Arc::new(Theme::default()));
        menu.show(
            "0123456789abcdef".to_string(),
            "0123456".to_string(),
            "subject".to_string(),
            false,
            false,
        );
        menu.rendered_area = Rect::new(0, 0, 40, 12);

        let action = menu
            .handle_mouse_event(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();

        assert!(matches!(action, Some(Action::CopyText(hash)) if hash == "0123456789abcdef"));
    }
}
