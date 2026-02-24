use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;

#[derive(Clone, Debug)]
enum MenuAction {
    OpenGraph,
    Refresh,
    CopyPath,
    Push,
    Pull,
    PullRebase,
}

struct MenuItem {
    label: String,
    action: MenuAction,
}

pub(crate) struct ContextMenu {
    pub visible: bool,
    pub repo_index: usize,
    pub position: (u16, u16), // (col, row)
    items: Vec<MenuItem>,
    state: ListState,
    last_rendered_area: Rect,
    action_tx: Option<UnboundedSender<Action>>,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            visible: false,
            repo_index: 0,
            position: (0, 0),
            items: Vec::new(),
            state: ListState::default(),
            last_rendered_area: Rect::default(),
            action_tx: None,
        }
    }

    pub fn show(&mut self, repo_index: usize, col: u16, row: u16, ahead: usize, behind: usize) {
        self.visible = true;
        self.repo_index = repo_index;
        self.position = (col, row);

        self.items = vec![
            MenuItem {
                label: "Open git graph".into(),
                action: MenuAction::OpenGraph,
            },
            MenuItem {
                label: "Refresh".into(),
                action: MenuAction::Refresh,
            },
            MenuItem {
                label: "Copy path".into(),
                action: MenuAction::CopyPath,
            },
        ];

        if ahead > 0 {
            self.items.push(MenuItem {
                label: format!("Push  ↑{}", ahead),
                action: MenuAction::Push,
            });
        }
        if behind > 0 {
            self.items.push(MenuItem {
                label: format!("Pull  ↓{}", behind),
                action: MenuAction::Pull,
            });
        }
        if ahead > 0 && behind > 0 {
            self.items.push(MenuItem {
                label: "Pull --rebase".into(),
                action: MenuAction::PullRebase,
            });
        }

        self.state.select(Some(0));
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    fn menu_rect(&self, terminal_area: Rect) -> Rect {
        let width = 22u16;
        let height = (self.items.len() as u16) + 2; // +2 for border

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
        if self.items.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => (i + 1).min(self.items.len() - 1),
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
        let item = self.items.get(idx)?;
        let action = match item.action {
            MenuAction::OpenGraph => Action::ShowGitGraph,
            MenuAction::Refresh => Action::RefreshRepo(self.repo_index),
            MenuAction::CopyPath => Action::CopyPath(self.repo_index),
            MenuAction::Push => Action::GitPush(self.repo_index),
            MenuAction::Pull => Action::GitPull(self.repo_index),
            MenuAction::PullRebase => Action::GitPullRebase(self.repo_index),
        };
        self.hide();
        Some(action)
    }

    fn click_item_index(&self, col: u16, row: u16) -> Option<usize> {
        let rect = self.menu_rect(self.last_rendered_area);
        let content_x = rect.x + 1;
        let content_y = rect.y + 1;
        let content_right = rect.x + rect.width.saturating_sub(1);
        let content_bottom = content_y + self.items.len() as u16;

        if col >= content_x && col < content_right && row >= content_y && row < content_bottom {
            Some((row - content_y) as usize)
        } else {
            None
        }
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
                Ok(Some(Action::HideContextMenu))
            }
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = self.click_item_index(mouse.column, mouse.row) {
                    self.state.select(Some(idx));
                    return Ok(self.activate_selected());
                }
                self.hide();
                Ok(None)
            }
            MouseEventKind::Down(_) => {
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

        self.last_rendered_area = area;
        let rect = self.menu_rect(area);

        frame.render_widget(Clear, rect);

        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| {
                let style = match item.action {
                    MenuAction::Push => Style::default().fg(Color::Green),
                    MenuAction::Pull | MenuAction::PullRebase => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                };
                ListItem::new(Line::from(Span::styled(&item.label, style)))
            })
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
