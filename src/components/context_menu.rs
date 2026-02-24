use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;

const MENU_ITEMS: &[(&str, MenuAction)] = &[
    ("Open git graph", MenuAction::OpenGraph),
    ("Refresh", MenuAction::Refresh),
    ("Copy path", MenuAction::CopyPath),
];

#[derive(Clone, Debug)]
enum MenuAction {
    OpenGraph,
    Refresh,
    CopyPath,
}

pub(crate) struct ContextMenu {
    pub visible: bool,
    pub repo_index: usize,
    pub position: (u16, u16), // (col, row)
    state: ListState,
    action_tx: Option<UnboundedSender<Action>>,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            visible: false,
            repo_index: 0,
            position: (0, 0),
            state: ListState::default(),
            action_tx: None,
        }
    }

    pub fn show(&mut self, repo_index: usize, col: u16, row: u16) {
        self.visible = true;
        self.repo_index = repo_index;
        self.position = (col, row);
        self.state.select(Some(0));
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    fn menu_rect(&self, terminal_area: Rect) -> Rect {
        let width = 20u16;
        let height = (MENU_ITEMS.len() as u16) + 2; // +2 for border

        let x = self
            .position
            .0
            .min(terminal_area.width.saturating_sub(width));
        let y = self
            .position
            .1
            .min(terminal_area.height.saturating_sub(height));

        Rect::new(x, y, width, height)
    }

    fn select_next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(MENU_ITEMS.len() - 1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn select_prev(&mut self) {
        let i = match self.state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn activate_selected(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        let (_, menu_action) = MENU_ITEMS.get(idx)?;
        let action = match menu_action {
            MenuAction::OpenGraph => Action::ShowGitGraph,
            MenuAction::Refresh => Action::RefreshRepo(self.repo_index),
            MenuAction::CopyPath => Action::CopyPath(self.repo_index),
        };
        self.hide();
        Some(action)
    }

    #[allow(dead_code)]
    pub fn hit_test(&self, col: u16, row: u16, terminal_area: Rect) -> bool {
        if !self.visible {
            return false;
        }
        let rect = self.menu_rect(terminal_area);
        col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
    }
}

impl Component for ContextMenu {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }

        match key.code {
            KeyCode::Esc => {
                self.hide();
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
            KeyCode::Enter => Ok(self.activate_selected()),
            _ => {
                self.hide();
                Ok(None)
            }
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check if click is inside menu
                let menu_rect = self.menu_rect(Rect::new(0, 0, 200, 200)); // approximate
                let rel_y = mouse.row.saturating_sub(menu_rect.y + 1); // -1 for border
                if rel_y < MENU_ITEMS.len() as u16 {
                    self.state.select(Some(rel_y as usize));
                    return Ok(self.activate_selected());
                }
                self.hide();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        if !self.visible {
            return Ok(());
        }

        let rect = self.menu_rect(area);

        // Clear the area behind the menu
        frame.render_widget(Clear, rect);

        let items: Vec<ListItem> = MENU_ITEMS
            .iter()
            .map(|(label, _)| ListItem::new(*label))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, rect, &mut self.state);
        Ok(())
    }
}
