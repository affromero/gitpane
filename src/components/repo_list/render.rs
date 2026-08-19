use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
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

/// Compute the absolute column ranges of the stash and worktree toggle
/// segments for a repo row, given the leftmost content column. Both this
/// hit-test and [`RepoList::render_repo_item`] walk the same
/// [`attention_cells`] from [`RowLayout::attention_x`] — keep the two in
/// sync.
pub(super) fn indicator_columns(
    entry: &RepoEntry,
    base_x: u16,
    layout: &RowLayout,
) -> IndicatorColumns {
    let mut out = IndicatorColumns::default();
    let Some(status) = entry.status.as_ref() else {
        return out;
    };
    let mut x = base_x.saturating_add(layout.attention_x());
    for (text, kind) in attention_cells(status, false, false) {
        let width = text.chars().count() as u16;
        match kind {
            AttentionCell::Stash => out.stash = Some((x, x + width)),
            AttentionCell::Worktree => out.worktree = Some((x, x + width)),
            _ => {}
        }
        x += width + 1;
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
            KeyCode::Char('f') => {
                self.focus_mode = !self.focus_mode;
                Ok(None)
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
                            let layout = row_layout(&self.repos, &self.live_panes, inner_width);
                            let cols = indicator_columns(&self.repos[*i], base_x, &layout);
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

        let inner_width = area.width.saturating_sub(2);
        let layout = row_layout(&self.repos, &self.live_panes, inner_width);
        let selected_repo = self
            .state
            .selected()
            .and_then(|s| self.display_rows.get(s))
            .map(|row| match row {
                DisplayRow::Repo(i) => *i,
                DisplayRow::Worktree(ri, _) | DisplayRow::Stash(ri, _) => *ri,
            });
        let items: Vec<ListItem> = self
            .display_rows
            .iter()
            .map(|row| {
                let (item, repo_idx) = match row {
                    DisplayRow::Repo(i) => (self.render_repo_item(&self.repos[*i], &layout), *i),
                    DisplayRow::Worktree(ri, wi) => {
                        (self.render_worktree_item(&self.repos[*ri], *wi), *ri)
                    }
                    DisplayRow::Stash(ri, si) => {
                        (self.render_stash_item(&self.repos[*ri], *si), *ri)
                    }
                };
                // Focus mode: dim every repo but the selected one; the
                // selected repo's worktree/stash rows stay bright too.
                if self.focus_mode && selected_repo.is_some_and(|s| s != repo_idx) {
                    item.style(Style::default().add_modifier(Modifier::DIM))
                } else {
                    item
                }
            })
            .collect();

        let t = &self.theme.repo_list;
        let border_color = if self.focused {
            t.border_focused
        } else {
            t.border_unfocused
        };

        // Persistent warning for configured roots that don't exist on disk.
        // Discovery silently skips them, so without this the workspace can
        // shrink (or vanish) with no explanation. Rendered above the list for
        // the whole session — not a toast that expires after a few seconds.
        let missing_hint = if self.missing_roots.is_empty() {
            None
        } else {
            let names = self
                .missing_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("root does not exist: {names}"))
        };
        let list_area = if let Some(text) = missing_hint {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(area);
            let s = &self.theme.status_bar;
            let label = Span::styled(
                " ! ",
                Style::default()
                    .fg(s.error_label_fg)
                    .bg(s.error_label_bg)
                    .add_modifier(Modifier::BOLD),
            );
            // The rendered line is the 3-cell " ! " label plus the text;
            // give the ellipsize budget the text's share so the ellipsis
            // marker itself isn't clipped by the paragraph.
            let text = middle_ellipsize(&text, area.width.saturating_sub(3) as usize);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    label,
                    Span::styled(text, Style::default().fg(s.error_text)),
                ])),
                rows[0],
            );
            rows[1]
        } else {
            area
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

        frame.render_stateful_widget(list, list_area, &mut self.state);
        Ok(())
    }
}
