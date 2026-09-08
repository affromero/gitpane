use crossterm::event::{KeyCode, KeyEvent};

use super::{CommitDetail, GitGraph};
use crate::action::Action;

impl CommitDetail {
    pub(super) fn file_matches(&self, index: usize) -> bool {
        !self.file_filter.is_active() || self.file_filter.position_of(index).is_some()
    }

    pub(super) fn step_file(&mut self, delta: isize) {
        if self.file_searching || self.file_filter.is_active() {
            self.file_state
                .select(self.file_filter.step(self.file_state.selected(), delta));
            return;
        }
        if self.files.is_empty() {
            return;
        }
        let next = self
            .file_state
            .selected()
            .map(|i| i.saturating_add_signed(delta).min(self.files.len() - 1))
            .unwrap_or(0);
        self.file_state.select(Some(next));
    }
}

impl GitGraph {
    pub(super) fn file_selection_changed(&mut self) -> Option<Action> {
        if self.commit_detail.as_ref()?.file_searching {
            return self.file_search_selection_changed();
        }
        self.schedule_commit_diff()
    }

    pub fn file_search_active(&self) -> bool {
        self.commit_detail
            .as_ref()
            .is_some_and(|d| d.file_searching && !d.diff_focused)
    }

    pub(super) fn start_file_search(&mut self) {
        if let Some(detail) = self.commit_detail.as_mut() {
            detail.file_searching = true;
            detail.file_filter.rebuild(&detail.file_paths);
        }
    }

    pub(super) fn handle_file_search_key(&mut self, key: KeyEvent) -> Option<Action> {
        let detail = self.commit_detail.as_mut()?;
        match key.code {
            KeyCode::Char(c) => detail.file_filter.push(c),
            KeyCode::Backspace => detail.file_filter.pop(),
            KeyCode::Esc => {
                detail.file_filter.clear();
                detail.file_searching = false;
            }
            KeyCode::Down | KeyCode::Up => {
                detail.step_file(if key.code == KeyCode::Down { 1 } else { -1 });
                return self.file_search_selection_changed();
            }
            KeyCode::Enter => {
                let action = self.try_show_commit_diff();
                if action.is_some() {
                    self.commit_detail.as_mut()?.diff_focused = true;
                }
                return action;
            }
            _ => return None,
        }
        detail.file_filter.rebuild(&detail.file_paths);
        detail.file_state.select(
            detail
                .file_filter
                .selected_match(detail.file_state.selected()),
        );
        self.file_search_selection_changed()
    }

    fn file_search_selection_changed(&mut self) -> Option<Action> {
        let detail = self.commit_detail.as_mut()?;
        detail.diff_content = None;
        detail.diff_scroll = 0;
        // Reject a diff already loading for the previous selection as well as
        // the previous selection's pending debounce.
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.schedule_commit_diff()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Component;
    use crate::theme::Theme;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    fn graph() -> GitGraph {
        let mut graph = GitGraph::new(Arc::new(Theme::default()));
        graph.repo_path = Some("/repo".into());
        assert!(
            graph
                .set_commit_files(
                    "abc1234".into(),
                    "subject".into(),
                    ["main.rs", "lib.rs", "other.rs", "library.rs"]
                        .into_iter()
                        .map(|p| ("M".into(), p.into()))
                        .collect()
                )
                .is_some()
        );
        graph
    }

    fn search(graph: &mut GitGraph, query: &str) {
        graph.handle_key_event(KeyCode::Char('/').into()).unwrap();
        for c in query.chars() {
            graph.handle_key_event(KeyCode::Char(c).into()).unwrap();
        }
    }

    #[test]
    fn commit_search_opens_matching_path_and_escape_keeps_detail() {
        let mut graph = graph();
        search(&mut graph, "lib");
        assert!(
            matches!(graph.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowCommitDiff { file_path, .. }) if file_path == "lib.rs")
        );
        graph.handle_key_event(KeyCode::Esc.into()).unwrap();
        graph.handle_key_event(KeyCode::Esc.into()).unwrap();
        assert!(graph.has_detail());
        assert!(!graph.file_search_active());
        graph.handle_key_event(KeyCode::Esc.into()).unwrap();
        assert!(!graph.has_detail());
    }

    #[test]
    fn no_match_cancels_debounce_and_enter_until_query_is_cleared() {
        let mut graph = graph();
        search(&mut graph, "lib");
        let action = graph.handle_key_event(KeyCode::Down.into()).unwrap();
        let Some(Action::ScheduleCommitDiff { generation }) = action else {
            panic!("expected scheduled diff")
        };
        graph.handle_key_event(KeyCode::Char('z').into()).unwrap();
        assert!(graph.commit_diff_settled(generation).is_none());
        assert!(
            graph
                .handle_key_event(KeyCode::Enter.into())
                .unwrap()
                .is_none()
        );
        graph.handle_key_event(KeyCode::Backspace.into()).unwrap();
        assert!(
            matches!(graph.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowCommitDiff { file_path, .. }) if file_path == "lib.rs")
        );
    }

    #[test]
    fn wheel_moves_between_matching_commit_files() {
        let mut graph = graph();
        graph.horizontal_layout = true;
        search(&mut graph, "lib");
        let mut terminal = Terminal::new(TestBackend::new(100, 60)).unwrap();
        terminal.draw(|f| graph.draw(f, f.area()).unwrap()).unwrap();
        let area = graph.files_area;
        graph
            .handle_mouse_event(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: area.x + 1,
                row: area.y + 1,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();
        assert!(
            matches!(graph.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowCommitDiff { file_path, .. }) if file_path == "library.rs")
        );
    }
}
