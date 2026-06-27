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

const MENU_WIDTH: u16 = 26;

#[derive(Clone, Debug)]
enum MenuAction {
    Open,
    Review,
    /// Attach a single live session directly.
    GotoSession(String),
    /// Several live sessions: open the picker to choose.
    GotoSessionPicker,
    NewWorktree,
    RemoveWorktree,
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

/// A rendered menu row: a selectable item or a non-selectable group divider.
enum MenuRow {
    Item(MenuItem),
    Separator,
}

/// Row context that decides which items the menu offers.
#[derive(Clone)]
pub(crate) struct MenuContext {
    pub ahead: usize,
    pub behind: usize,
    pub has_submodules: bool,
    pub is_worktree: bool,
    /// tmux sessions live in this row's path; surfaced as session menu items.
    pub live_sessions: Vec<String>,
    /// The resolved `[goto] command`, used to label the session item with where
    /// it opens (new tab / new window).
    pub goto_command: String,
}

pub(crate) struct ContextMenu {
    pub visible: bool,
    pub repo_id: Option<RepoId>,
    pub position: (u16, u16), // (col, row)
    rows: Vec<MenuRow>,
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
            rows: Vec::new(),
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
            live_sessions,
            goto_command,
        } = ctx;
        self.visible = true;
        self.repo_id = Some(repo_id);
        self.position = (col, row);

        let item = |label: String, action: MenuAction| MenuItem { label, action };

        // Items are grouped by topic; groups are joined with separators so the
        // menu reads as sections instead of one long list.

        // Launch / inspect.
        let mut launch = vec![
            item("Open".into(), MenuAction::Open),
            item("Review changes".into(), MenuAction::Review),
        ];
        // The session item label says where it opens ("(new tab)"/"(new
        // window)"), inferred from the [goto] command, so it's clear the current
        // view stays put.
        let where_suffix = match crate::session::launcher::goto_placement(&goto_command) {
            Some(p) => format!(" ({p})"),
            None => String::new(),
        };
        match live_sessions.len() {
            0 => {}
            1 => {
                let s = live_sessions.into_iter().next().unwrap();
                launch.push(item(
                    format!("Open {s} active tmux{where_suffix}"),
                    MenuAction::GotoSession(s),
                ));
            }
            _ => launch.push(item(
                format!("Open active tmux session…{where_suffix}"),
                MenuAction::GotoSessionPicker,
            )),
        }
        launch.push(item("Open git graph".into(), MenuAction::OpenGraph));

        // Repo housekeeping.
        let housekeeping = vec![
            item("Refresh".into(), MenuAction::Refresh),
            item("Copy path".into(), MenuAction::CopyPath),
        ];

        // Sync (push/pull, plus submodules when present).
        let push_label = if ahead > 0 {
            format!("Push  ↑{ahead}")
        } else {
            "Push".into()
        };
        let pull_label = if behind > 0 {
            format!("Pull  ↓{behind}")
        } else {
            "Pull".into()
        };
        let mut sync = vec![
            item(push_label, MenuAction::Push),
            item(pull_label, MenuAction::Pull),
            item("Pull --rebase".into(), MenuAction::PullRebase),
        ];
        if has_submodules {
            sync.push(item(
                "Pull --recurse-subs".into(),
                MenuAction::PullSubmodules,
            ));
            sync.push(item(
                "Sub: update --init".into(),
                MenuAction::SubmoduleUpdate,
            ));
            sync.push(item("Sub: sync".into(), MenuAction::SubmoduleSync));
            sync.push(item(
                "Sub: pull latest".into(),
                MenuAction::SubmoduleUpdateLatest,
            ));
        }

        // Worktree management.
        let worktree = if is_worktree {
            vec![item("Remove worktree".into(), MenuAction::RemoveWorktree)]
        } else {
            vec![item("New worktree…".into(), MenuAction::NewWorktree)]
        };

        self.rows.clear();
        for group in [launch, housekeeping, sync, worktree] {
            if group.is_empty() {
                continue;
            }
            if !self.rows.is_empty() {
                self.rows.push(MenuRow::Separator);
            }
            self.rows.extend(group.into_iter().map(MenuRow::Item));
        }

        self.state.select(self.first_item_index());
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    fn first_item_index(&self) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, MenuRow::Item(_)))
    }

    /// Width that fits the longest item label (min `MENU_WIDTH`), so labels like
    /// "Open fairtrail active tmux (new window)" aren't truncated.
    fn menu_width(&self) -> u16 {
        let longest = self
            .rows
            .iter()
            .filter_map(|r| match r {
                MenuRow::Item(i) => Some(i.label.chars().count()),
                MenuRow::Separator => None,
            })
            .max()
            .unwrap_or(0) as u16;
        (longest + 4).max(MENU_WIDTH) // +2 border, +2 list padding
    }

    fn menu_rect(&self, terminal_area: Rect) -> Rect {
        let width = self.menu_width().min(terminal_area.width);
        let height = (self.rows.len() as u16) + 2; // +2 for border

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
        let cur = self.state.selected().unwrap_or(0);
        if let Some(next) =
            ((cur + 1)..self.rows.len()).find(|&i| matches!(self.rows[i], MenuRow::Item(_)))
        {
            self.state.select(Some(next));
        }
    }

    fn select_prev(&mut self) {
        let cur = self.state.selected().unwrap_or(0);
        if let Some(prev) = (0..cur)
            .rev()
            .find(|&i| matches!(self.rows[i], MenuRow::Item(_)))
        {
            self.state.select(Some(prev));
        }
    }

    fn activate_selected(&mut self) -> Option<Action> {
        let idx = self.state.selected()?;
        let MenuRow::Item(item) = self.rows.get(idx)? else {
            return None;
        };
        let id = self.repo_id.clone()?;
        // Every menu action is path-bound (carries the right-clicked row's id) so
        // an async row re-sort while the menu is open can't retarget a different
        // repo/worktree than was clicked.
        let action = match item.action {
            MenuAction::Open => Action::OpenAt(id),
            MenuAction::Review => Action::ReviewAt(id),
            MenuAction::GotoSession(ref s) => Action::GotoSession(s.clone()),
            MenuAction::GotoSessionPicker => Action::GotoSessionPicker(id),
            MenuAction::NewWorktree => Action::OpenNewWorktree(id),
            MenuAction::RemoveWorktree => Action::RemoveWorktreeAt(id),
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
        let content_bottom = content_y + self.rows.len() as u16;

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
                    // Clicking a separator does nothing; only items activate.
                    if matches!(self.rows.get(idx), Some(MenuRow::Item(_))) {
                        self.state.select(Some(idx));
                        return Ok(self.activate_selected());
                    }
                    return Ok(None);
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
        let divider = "─".repeat((rect.width.saturating_sub(2)) as usize);
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| match row {
                MenuRow::Separator => ListItem::new(Line::from(Span::styled(
                    divider.clone(),
                    Style::default()
                        .fg(t.context_menu_border)
                        .add_modifier(Modifier::DIM),
                ))),
                MenuRow::Item(item) => {
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
                }
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
