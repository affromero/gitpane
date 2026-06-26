use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::repo_id::RepoId;
use crate::theme::Theme;

#[derive(Clone, Debug)]
enum MenuAction {
    Open,
    Review,
    NewWorktree,
    OpenGraph,
    Refresh,
    CopyPath,
    Push,
    Pull,
    PullRebase,
    PullSubmodules,
    SubmoduleUpdate,
    SubmoduleSync,
    SubmoduleUpdateLatest,
}

struct MenuItem {
    label: String,
    action: MenuAction,
}

/// Row context that decides which items the menu offers.
#[derive(Clone, Copy)]
pub(crate) struct MenuContext {
    pub ahead: usize,
    pub behind: usize,
    pub has_submodules: bool,
    pub is_worktree: bool,
}

pub(crate) struct ContextMenu {
    pub visible: bool,
    pub repo_id: Option<RepoId>,
    pub position: (u16, u16), // (col, row)
    items: Vec<MenuItem>,
    state: ListState,
    last_rendered_area: Rect,
    action_tx: Option<UnboundedSender<Action>>,
    theme: Arc<Theme>,
}

impl ContextMenu {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            repo_id: None,
            position: (0, 0),
            items: Vec::new(),
            state: ListState::default(),
            last_rendered_area: Rect::default(),
            action_tx: None,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn show(&mut self, repo_id: RepoId, col: u16, row: u16, ctx: MenuContext) {
        let MenuContext {
            ahead,
            behind,
            has_submodules,
            is_worktree,
        } = ctx;
        self.visible = true;
        self.repo_id = Some(repo_id);
        self.position = (col, row);

        self.items = vec![
            MenuItem {
                label: "Open".into(),
                action: MenuAction::Open,
            },
            MenuItem {
                label: "Review changes".into(),
                action: MenuAction::Review,
            },
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
            MenuItem {
                label: if ahead > 0 {
                    format!("Push  ↑{}", ahead)
                } else {
                    "Push".into()
                },
                action: MenuAction::Push,
            },
            MenuItem {
                label: if behind > 0 {
                    format!("Pull  ↓{}", behind)
                } else {
                    "Pull".into()
                },
                action: MenuAction::Pull,
            },
            MenuItem {
                label: "Pull --rebase".into(),
                action: MenuAction::PullRebase,
            },
        ];

        // Worktree-specific items. "New worktree…" creates a worktree of the
        // repo on a repo row; worktree rows get worktree-management items.
        if !is_worktree {
            self.items.push(MenuItem {
                label: "New worktree…".into(),
                action: MenuAction::NewWorktree,
            });
        }

        if has_submodules {
            self.items.push(MenuItem {
                label: "Pull --recurse-subs".into(),
                action: MenuAction::PullSubmodules,
            });
            self.items.push(MenuItem {
                label: "Sub: update --init".into(),
                action: MenuAction::SubmoduleUpdate,
            });
            self.items.push(MenuItem {
                label: "Sub: sync".into(),
                action: MenuAction::SubmoduleSync,
            });
            self.items.push(MenuItem {
                label: "Sub: pull latest".into(),
                action: MenuAction::SubmoduleUpdateLatest,
            });
        }

        self.state.select(Some(0));
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    fn menu_rect(&self, terminal_area: Rect) -> Rect {
        let width = 24u16;
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
        let id = self.repo_id.clone()?;
        let action = match item.action {
            MenuAction::Open => Action::OpenSelected,
            MenuAction::Review => Action::ReviewSelected,
            MenuAction::NewWorktree => Action::OpenNewWorktree(id),
            MenuAction::OpenGraph => Action::ShowGitGraph,
            MenuAction::Refresh => Action::RefreshRepo(id),
            MenuAction::CopyPath => Action::CopyPath(id),
            MenuAction::Push => Action::GitPush(id),
            MenuAction::Pull => Action::GitPull(id),
            MenuAction::PullRebase => Action::GitPullRebase(id),
            MenuAction::PullSubmodules => Action::GitPullSubmodules(id),
            MenuAction::SubmoduleUpdate => Action::GitSubmoduleUpdate(id),
            MenuAction::SubmoduleSync => Action::GitSubmoduleSync(id),
            MenuAction::SubmoduleUpdateLatest => Action::GitSubmoduleUpdateLatest(id),
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

        let t = &self.theme.overlay;
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| {
                let style = match item.action {
                    MenuAction::Push => Style::default().fg(t.context_menu_push),
                    MenuAction::Pull | MenuAction::PullRebase | MenuAction::PullSubmodules => {
                        Style::default().fg(t.context_menu_pull)
                    }
                    MenuAction::SubmoduleUpdate
                    | MenuAction::SubmoduleSync
                    | MenuAction::SubmoduleUpdateLatest => {
                        Style::default().fg(t.context_menu_submodule)
                    }
                    _ => Style::default(),
                };
                ListItem::new(Line::from(Span::styled(&item.label, style)))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.context_menu_border)),
            )
            .highlight_style(
                Style::default()
                    .bg(t.context_menu_selection_bg)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, rect, &mut self.state);
        Ok(())
    }
}
