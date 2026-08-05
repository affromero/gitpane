use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, List, ListItem},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;
use crate::components::Component;
use crate::repo_id::RepoId;

use super::*;

/// Half-open column ranges `[start, end)` of the subtree indicators rendered
/// on a repo row. `None` means the indicator is not drawn for this entry.
/// Used by the click handler to route a click to the right toggle.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IndicatorColumns {
    pub(super) stash: Option<(u16, u16)>,
    pub(super) worktree: Option<(u16, u16)>,
}

/// Compute the column ranges of the stash and worktree indicators for a repo
/// row, given the leftmost content column. Mirrors the layout in
/// [`RepoList::render_repo_item`] — keep the two in sync.
pub(super) fn indicator_columns(
    entry: &RepoEntry,
    base_x: u16,
    name_width: u16,
) -> IndicatorColumns {
    let mut col = base_x;
    // Dirty/git_op marker: always 2 columns ("* ", "~ ", or "  ").
    col = col.saturating_add(2);
    // The repo display name (path relative to the workspace root) sits
    // between the marker and the branch; it is variable width.
    col = col.saturating_add(name_width);

    let mut out = IndicatorColumns::default();
    let Some(status) = entry.status.as_ref() else {
        return out;
    };
    // Branch field is left-padded to a minimum of 12 columns, then a space.
    let branch_width = status.branch.chars().count().max(12).saturating_add(1);
    col = col.saturating_add(branch_width as u16);

    if status.ahead > 0 {
        // "↑{N} " — chevron (1) + digits + trailing space.
        col = col.saturating_add(2 + status.ahead.to_string().len() as u16);
    }
    if status.behind > 0 {
        col = col.saturating_add(2 + status.behind.to_string().len() as u16);
    }

    if !status.stashes.is_empty() {
        let start = col;
        // "▶$N " — chevron (1) + '$' (1) + digits + trailing space.
        let width = 3 + status.stash_count().to_string().len() as u16;
        out.stash = Some((start, start.saturating_add(width)));
        col = col.saturating_add(width);
    }

    if !status.worktree_info.is_empty() {
        let start = col;
        // "▶N " — chevron (1) + digits + trailing space.
        let width = 2 + status.worktree_info.len().to_string().len() as u16;
        out.worktree = Some((start, start.saturating_add(width)));
    }

    out
}

impl Component for RepoList {
    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('w') => {
                self.toggle_expand();
                Ok(self.emit_selection_action())
            }
            KeyCode::Char('S') => {
                self.toggle_stash_expand();
                Ok(self.emit_selection_action())
            }
            _ => Ok(None),
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<Option<Action>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.display_rows.len() {
                        // Click on the already-selected repo row toggles a subtree.
                        // When both worktrees and stashes are present we hit-test
                        // the click column against each indicator's range so the
                        // user can target either. A click elsewhere on the row
                        // falls back to whichever subtree exists (worktrees first).
                        if self.state.selected() == Some(idx)
                            && let Some(DisplayRow::Repo(i)) = self.display_rows.get(idx)
                            && let Some(status) = self.repos[*i].status.as_ref()
                        {
                            let base_x = self.render_area.x + 1;
                            let inner_width = self.render_area.width.saturating_sub(2);
                            let name_width =
                                rendered_name(&self.repos[*i], inner_width).chars().count() as u16;
                            let cols = indicator_columns(&self.repos[*i], base_x, name_width);
                            let clicked_stash = cols
                                .stash
                                .is_some_and(|(s, e)| mouse.column >= s && mouse.column < e);
                            let clicked_worktree = cols
                                .worktree
                                .is_some_and(|(s, e)| mouse.column >= s && mouse.column < e);
                            if clicked_stash {
                                self.toggle_stash_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if clicked_worktree {
                                self.toggle_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if !status.worktree_info.is_empty() {
                                self.toggle_expand();
                                return Ok(self.emit_selection_action());
                            }
                            if !status.stashes.is_empty() {
                                self.toggle_stash_expand();
                                return Ok(self.emit_selection_action());
                            }
                        }
                        self.state.select(Some(idx));
                        return Ok(self.emit_selection_action());
                    }
                }
                Ok(None)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let content_y = self.render_area.y + 1;
                if mouse.column >= self.render_area.x
                    && mouse.column < self.render_area.x + self.render_area.width
                    && mouse.row >= content_y
                {
                    let visual_row = (mouse.row - content_y) as usize;
                    let idx = visual_row + self.state.offset();
                    if idx < self.display_rows.len() {
                        self.state.select(Some(idx));
                        // Repo and worktree rows get a context menu. A worktree
                        // menu targets the worktree's own path so pull/push act
                        // on it directly. Stash rows have no menu.
                        let menu = match self.display_rows.get(idx) {
                            Some(DisplayRow::Repo(i)) => {
                                Some((RepoId(self.repos[*i].path.clone()), false))
                            }
                            Some(DisplayRow::Worktree(ri, wi)) => self.repos[*ri]
                                .status
                                .as_ref()
                                .and_then(|s| s.worktree_info.get(*wi))
                                .map(|wt| (RepoId(wt.path.clone()), true)),
                            _ => None,
                        };
                        if let Some((id, is_worktree)) = menu {
                            return Ok(Some(Action::ShowContextMenu {
                                id,
                                row: mouse.row,
                                col: mouse.column,
                                is_worktree,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            MouseEventKind::ScrollUp => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            _ => Ok(None),
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::SelectNextRepo => {
                self.select_next();
                Ok(self.emit_selection_action())
            }
            Action::SelectPrevRepo => {
                self.select_prev();
                Ok(self.emit_selection_action())
            }
            Action::RepoStatusUpdated { ref id, ref status } => {
                if let Some(idx) = self.resolve_index(id) {
                    self.update_status(idx, status.clone());
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        self.render_area = area;
        // Ensure display_rows is fresh
        self.rebuild_display_rows();

        let items: Vec<ListItem> = self
            .display_rows
            .iter()
            .map(|row| match row {
                DisplayRow::Repo(i) => self.render_repo_item(&self.repos[*i], *i),
                DisplayRow::Worktree(ri, wi) => self.render_worktree_item(&self.repos[*ri], *wi),
                DisplayRow::Stash(ri, si) => self.render_stash_item(&self.repos[*ri], *si),
            })
            .collect();

        let t = &self.theme.repo_list;
        let border_color = if self.focused {
            t.border_focused
        } else {
            t.border_unfocused
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Repositories ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, area, &mut self.state);
        Ok(())
    }
}
