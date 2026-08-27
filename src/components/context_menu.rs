use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use std::path::PathBuf;
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
    // File-row actions (menu opened via `show_file`).
    OpenFile,
    RevealFile,
    CopyFilePath,
    /// Submodule row only: retarget the graph/changes panels onto the submodule.
    OpenSubmoduleGraph,
    /// Submodule row only: pin the submodule as its own entry in the repo list.
    AddSubmoduleAsRepo,
    StageFile,
    UnstageFile,
    /// Discard a changed file; `bool` is `is_untracked` (delete vs restore).
    DiscardFile(bool),
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
    /// Sessions/tabs live in this row's path; surfaced as session menu items.
    pub live_sessions: Vec<String>,
    /// The resolved `[goto] command`, used to label the session item with where
    /// it opens (new tab / new window).
    pub goto_command: String,
    /// Which multiplexer this runs under, so live-session items say "tmux" or
    /// "herdr tab" and the attach suffix matches the backend.
    pub mux: crate::session::env::Multiplexer,
}

/// File-row context that decides which file actions the menu offers.
pub(crate) struct FileMenuContext {
    pub path: PathBuf,
    /// Index side dirty: offer Unstage.
    pub staged: bool,
    /// Worktree side dirty (or conflicted): offer Stage.
    pub unstaged: bool,
    /// Untracked file: Discard means delete, not restore.
    pub is_untracked: bool,
    /// Submodule row: no stage/unstage/discard (only Open / Open folder).
    pub is_submodule: bool,
}

pub(crate) struct ContextMenu {
    pub visible: bool,
    pub repo_id: Option<RepoId>,
    /// Set in file mode (`show_file`); the right-clicked file's repo-relative
    /// path. `None` in repo mode (`show`).
    pub file_path: Option<PathBuf>,
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
            file_path: None,
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
            mux,
        } = ctx;
        self.visible = true;
        self.repo_id = Some(repo_id);
        self.file_path = None;
        self.position = (col, row);

        let item = |label: String, action: MenuAction| MenuItem { label, action };

        // Items are grouped by topic; groups are joined with separators so the
        // menu reads as sections instead of one long list.

        // Launch / inspect.
        let mut launch = vec![
            item("Open".into(), MenuAction::Open),
            item("Review changes".into(), MenuAction::Review),
        ];
        // The session item label says what is live and where it opens: tmux
        // sessions infer "(new tab)"/"(new window)" from the [goto] command;
        // herdr tabs focus in place.
        let (live_kind, live_plural) = match mux {
            crate::session::env::Multiplexer::Herdr => ("herdr tab", "herdr tabs"),
            _ => ("tmux", "tmux sessions"),
        };
        let where_suffix = match mux {
            crate::session::env::Multiplexer::Herdr => " (focus tab)".to_string(),
            _ => match crate::session::launcher::goto_placement(&goto_command) {
                Some(p) => format!(" ({p})"),
                None => String::new(),
            },
        };
        match live_sessions.len() {
            0 => {}
            1 => {
                let s = live_sessions.into_iter().next().unwrap();
                launch.push(item(
                    format!("Open {s} active {live_kind}{where_suffix}"),
                    MenuAction::GotoSession(s),
                ));
            }
            _ => launch.push(item(
                format!("Open active {live_plural}…{where_suffix}"),
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

    /// Open the menu for a changed-file row. Mutation items are gated by what the
    /// row supports (see [`FileMenuContext`]); Open / Open folder are always
    /// available. The menu is path-bound, so an async row re-sort can't retarget.
    pub fn show_file(&mut self, repo_id: RepoId, col: u16, row: u16, ctx: FileMenuContext) {
        let FileMenuContext {
            path,
            staged,
            unstaged,
            is_untracked,
            is_submodule,
        } = ctx;
        self.visible = true;
        self.repo_id = Some(repo_id);
        self.file_path = Some(path);
        self.position = (col, row);

        let item = |label: String, action: MenuAction| MenuItem { label, action };

        // Mutations, gated. Submodule rows get none: staging a submodule pointer
        // from the file menu would surprise more than help.
        let mut mutate = Vec::new();
        if !is_submodule {
            if unstaged {
                mutate.push(item("Stage".into(), MenuAction::StageFile));
            }
            if staged {
                mutate.push(item("Unstage".into(), MenuAction::UnstageFile));
            }
            if staged || unstaged {
                let label = if is_untracked {
                    "Delete file"
                } else {
                    "Discard changes"
                };
                mutate.push(item(label.into(), MenuAction::DiscardFile(is_untracked)));
            }
        }

        // Always available: inspect the file or its enclosing folder.
        let mut inspect = vec![
            item("Open".into(), MenuAction::OpenFile),
            item("Open folder".into(), MenuAction::RevealFile),
            item("Copy path".into(), MenuAction::CopyFilePath),
        ];
        // A submodule is its own repo: offer to browse its graph in the panels.
        if is_submodule {
            inspect.push(item("Open in graph".into(), MenuAction::OpenSubmoduleGraph));
            inspect.push(item(
                "Add to repositories".into(),
                MenuAction::AddSubmoduleAsRepo,
            ));
        }

        self.rows.clear();
        for group in [mutate, inspect] {
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
            // File actions carry the right-clicked path; `?` makes them no-ops if
            // the menu is somehow in repo mode (no file_path).
            MenuAction::OpenFile => Action::OpenFile(id, self.file_path.clone()?),
            MenuAction::RevealFile => Action::RevealFile(id, self.file_path.clone()?),
            MenuAction::CopyFilePath => Action::CopyFilePath(id, self.file_path.clone()?),
            MenuAction::OpenSubmoduleGraph => Action::SelectSubmodule {
                repo_id: id,
                sub_path: self.file_path.clone()?,
            },
            MenuAction::AddSubmoduleAsRepo => Action::AddRepo(id.0.join(self.file_path.clone()?)),
            MenuAction::StageFile => Action::StageFile(id, self.file_path.clone()?),
            MenuAction::UnstageFile => Action::UnstageFile(id, self.file_path.clone()?),
            MenuAction::DiscardFile(is_untracked) => {
                Action::DiscardFile(id, self.file_path.clone()?, is_untracked)
            }
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

#[cfg(test)]
mod file_menu_tests {
    use super::*;

    fn item_labels(menu: &ContextMenu) -> Vec<String> {
        menu.rows
            .iter()
            .filter_map(|r| match r {
                MenuRow::Item(i) => Some(i.label.clone()),
                MenuRow::Separator => None,
            })
            .collect()
    }

    fn show(staged: bool, unstaged: bool, is_untracked: bool, is_submodule: bool) -> ContextMenu {
        let mut menu = ContextMenu::new(Arc::new(Theme::default()));
        menu.show_file(
            RepoId(PathBuf::from("/repo")),
            0,
            0,
            FileMenuContext {
                path: PathBuf::from("a.txt"),
                staged,
                unstaged,
                is_untracked,
                is_submodule,
            },
        );
        menu
    }

    #[test]
    fn untracked_offers_stage_and_delete_but_not_unstage() {
        let labels = item_labels(&show(false, true, true, false));
        assert!(labels.contains(&"Stage".to_string()));
        assert!(labels.contains(&"Delete file".to_string()));
        assert!(!labels.iter().any(|l| l == "Unstage"));
        assert!(!labels.iter().any(|l| l == "Discard changes"));
        assert!(labels.contains(&"Open".to_string()));
        assert!(labels.contains(&"Open folder".to_string()));
    }

    #[test]
    fn staged_only_offers_unstage_not_stage() {
        let labels = item_labels(&show(true, false, false, false));
        assert!(labels.contains(&"Unstage".to_string()));
        assert!(!labels.iter().any(|l| l == "Stage"));
        assert!(labels.contains(&"Discard changes".to_string()));
    }

    #[test]
    fn staged_and_unstaged_offers_both_plus_discard() {
        let labels = item_labels(&show(true, true, false, false));
        assert!(labels.contains(&"Stage".to_string()));
        assert!(labels.contains(&"Unstage".to_string()));
        assert!(labels.contains(&"Discard changes".to_string()));
    }

    #[test]
    fn submodule_row_offers_inspect_and_graph_no_mutations() {
        let labels = item_labels(&show(false, true, false, true));
        // A submodule is its own repo: inspect + browse its graph, never
        // stage/unstage/discard the pointer.
        assert_eq!(
            labels,
            vec![
                "Open".to_string(),
                "Open folder".to_string(),
                "Copy path".to_string(),
                "Open in graph".to_string(),
                "Add to repositories".to_string(),
            ]
        );
    }
}
