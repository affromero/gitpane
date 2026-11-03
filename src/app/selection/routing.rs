use super::super::*;
use crossterm::event::MouseEvent;
use ratatui::layout::Position;

impl App {
    pub(in crate::app) fn selection_modal_visible(&self) -> bool {
        self.confirm_dialog.visible
            || self.path_input.visible
            || self.theme_picker.visible
            || self.graph_filter_picker.visible
            || self.graph_context_menu.visible
            || self.context_menu.visible
            || self.picker.visible
            || self.show_help
    }

    pub(in crate::app) fn text_selection_blocked(&self) -> bool {
        self.selection_modal_visible()
            || self.git_graph.search_visible()
            || self.file_list.search_active()
            || self.git_graph.file_search_active()
    }

    pub(in crate::app) fn cancel_mouse_gestures(&mut self) {
        self.text_selection.clear();
        self.dragging_border = None;
        self.repo_list.cancel_drag();
        self.file_list.cancel_drag();
        self.git_graph.cancel_drag();
        self.github_panel.cancel_drag();
    }

    pub(in crate::app) fn text_selection_area(&mut self, position: Position) -> Option<Rect> {
        if self.repo_area.contains(position) {
            let content = crate::components::scroll_pane::bordered_inner(self.repo_area);
            content.contains(position).then_some(content)
        } else if self.changes_area.contains(position) {
            self.file_list.text_selection_area(position)
        } else if self.graph_area.contains(position) {
            self.git_graph.text_selection_area(position)
        } else if self.github_visible && self.github_area.contains(position) {
            self.github_panel.text_selection_area(position)
        } else {
            None
        }
    }

    pub(in crate::app) fn draw_text_selection(&mut self, frame: &mut ratatui::Frame) -> Result<()> {
        if self.text_selection_blocked() {
            self.text_selection.clear();
            if self.selection_modal_visible() {
                self.cancel_mouse_gestures();
            }
            return Ok(());
        }
        let area = self
            .text_selection
            .origin()
            .and_then(|position| self.text_selection_area(position));
        self.text_selection.draw(frame.buffer_mut(), area);
        if let Some(text) = self.text_selection.take_copy() {
            self.action_tx.send(Action::CopyText(text))?;
        }
        Ok(())
    }

    pub(in crate::app) fn route_panel_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let pos = Position::new(mouse.column, mouse.row);
        let action = if self.repo_area.contains(pos) {
            self.repo_list.handle_mouse_event(mouse)?
        } else if self.changes_area.contains(pos) {
            self.file_list.handle_mouse_event(mouse)?
        } else if self.graph_area.contains(pos) {
            self.git_graph.handle_mouse_event(mouse)?
        } else if self.github_visible && self.github_area.contains(pos) {
            self.github_panel.handle_mouse_event(mouse)?
        } else {
            None
        };
        if let Some(action) = action {
            self.action_tx.send(action)?;
        }
        Ok(())
    }
}
