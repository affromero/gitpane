use std::collections::BTreeSet;
use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

use crate::action::Action;
use crate::git::graph::GraphFilters;
use crate::theme::Theme;

#[derive(Clone, Copy)]
enum Category {
    Branches,
    Authors,
    Refs,
    Views,
}

const REF_CHOICES: [&str; 4] = ["Local branches", "Remote branches", "Tags", "Stashes"];
const VIEW_CHOICES: [&str; 1] = ["First-parent only"];

/// Modal multi-select picker for graph branches and authors.
pub(crate) struct GraphFilterPicker {
    pub visible: bool,
    filters: GraphFilters,
    branches: Vec<String>,
    authors: Vec<String>,
    first_parent: bool,
    category: Option<Category>,
    state: ListState,
    rendered_area: Rect,
    theme: Arc<Theme>,
}

impl GraphFilterPicker {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            visible: false,
            filters: GraphFilters::default(),
            branches: Vec::new(),
            authors: Vec::new(),
            first_parent: false,
            category: None,
            state: ListState::default(),
            rendered_area: Rect::default(),
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.category = None;
        self.state.select(None);
    }

    pub fn show(
        &mut self,
        filters: GraphFilters,
        branches: Vec<String>,
        authors: Vec<String>,
        first_parent: bool,
    ) {
        self.filters = filters;
        self.branches = branches;
        self.authors = authors;
        self.first_parent = first_parent;
        self.category = None;
        self.state.select(Some(0));
        self.visible = true;
    }

    fn choices(&self) -> Vec<String> {
        match self.category {
            Some(Category::Branches) => self.branches.clone(),
            Some(Category::Authors) => self.authors.clone(),
            Some(Category::Refs) => REF_CHOICES.iter().map(ToString::to_string).collect(),
            Some(Category::Views) => VIEW_CHOICES.iter().map(ToString::to_string).collect(),
            None => Vec::new(),
        }
    }

    fn selected_set(&self) -> Option<&BTreeSet<String>> {
        match self.category {
            Some(Category::Branches) => self.filters.branches.as_ref(),
            Some(Category::Authors) => self.filters.authors.as_ref(),
            Some(Category::Refs | Category::Views) | None => None,
        }
    }

    fn set_selected_set(&mut self, values: Option<BTreeSet<String>>) {
        match self.category {
            Some(Category::Branches) => self.filters.branches = values,
            Some(Category::Authors) => self.filters.authors = values,
            Some(Category::Refs | Category::Views) | None => {}
        }
    }

    fn selection_summary(values: Option<&BTreeSet<String>>, total: usize) -> String {
        match values {
            None => "all".to_string(),
            Some(values) => format!("{}/{}", values.len(), total),
        }
    }

    fn select_next(&mut self, len: usize) {
        if len > 0 {
            self.state.select(Some(
                self.state.selected().map_or(0, |i| (i + 1).min(len - 1)),
            ));
        }
    }

    fn select_prev(&mut self) {
        self.state
            .select(Some(self.state.selected().unwrap_or(0).saturating_sub(1)));
    }

    fn apply(&self) -> Action {
        Action::SetGraphFilters(self.filters.clone())
    }

    fn toggle_choice(&mut self, index: usize) -> Option<Action> {
        let choices = self.choices();
        let choice = choices.get(index)?.clone();
        if matches!(self.category, Some(Category::Refs)) {
            match index {
                0 => self.filters.refs.local = !self.filters.refs.local,
                1 => self.filters.refs.remote = !self.filters.refs.remote,
                2 => self.filters.refs.tags = !self.filters.refs.tags,
                3 => self.filters.refs.stashes = !self.filters.refs.stashes,
                _ => return None,
            }
            return Some(self.apply());
        }
        if matches!(self.category, Some(Category::Views)) {
            self.first_parent = !self.first_parent;
            return Some(Action::SetGraphFirstParent(self.first_parent));
        }
        let all: BTreeSet<String> = choices.into_iter().collect();
        let mut selected = self.selected_set().cloned().unwrap_or(all);
        if !selected.insert(choice.clone()) {
            selected.remove(&choice);
        }
        self.set_selected_set(Some(selected));
        Some(self.apply())
    }

    fn activate_category_row(&mut self) -> Option<Action> {
        match self.state.selected().unwrap_or(0) {
            0 => Some(self.set_all()),
            1 => Some(self.set_none()),
            2 => Some(self.invert()),
            index => self.toggle_choice(index - 3),
        }
    }

    fn set_all(&mut self) -> Action {
        if matches!(self.category, Some(Category::Refs)) {
            self.filters.refs = Default::default();
            return self.apply();
        }
        if matches!(self.category, Some(Category::Views)) {
            self.first_parent = true;
            return Action::SetGraphFirstParent(true);
        }
        self.set_selected_set(None);
        self.apply()
    }

    fn set_none(&mut self) -> Action {
        if matches!(self.category, Some(Category::Refs)) {
            self.filters.refs.local = false;
            self.filters.refs.remote = false;
            self.filters.refs.tags = false;
            self.filters.refs.stashes = false;
            return self.apply();
        }
        if matches!(self.category, Some(Category::Views)) {
            self.first_parent = false;
            return Action::SetGraphFirstParent(false);
        }
        self.set_selected_set(Some(BTreeSet::new()));
        self.apply()
    }

    fn invert(&mut self) -> Action {
        if matches!(self.category, Some(Category::Refs)) {
            self.filters.refs.local = !self.filters.refs.local;
            self.filters.refs.remote = !self.filters.refs.remote;
            self.filters.refs.tags = !self.filters.refs.tags;
            self.filters.refs.stashes = !self.filters.refs.stashes;
            return self.apply();
        }
        if matches!(self.category, Some(Category::Views)) {
            self.first_parent = !self.first_parent;
            return Action::SetGraphFirstParent(self.first_parent);
        }
        let all: BTreeSet<String> = self.choices().into_iter().collect();
        let selected = self.selected_set().cloned().unwrap_or_else(|| all.clone());
        self.set_selected_set(Some(all.difference(&selected).cloned().collect()));
        self.apply()
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if !self.visible {
            return Ok(None);
        }

        if self.category.is_none() {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.select_next(5),
                KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
                KeyCode::Enter | KeyCode::Right => {
                    if self.state.selected() == Some(4) {
                        self.filters = GraphFilters::default();
                        self.first_parent = false;
                        return Ok(Some(Action::ResetGraphFilters));
                    }
                    self.category = Some(match self.state.selected().unwrap_or(0) {
                        1 => Category::Authors,
                        2 => Category::Refs,
                        3 => Category::Views,
                        _ => Category::Branches,
                    });
                    self.state.select(Some(0));
                }
                KeyCode::Char('r') => {
                    self.filters = GraphFilters::default();
                    self.first_parent = false;
                    return Ok(Some(Action::ResetGraphFilters));
                }
                KeyCode::Left => return Ok(Some(Action::OpenGraphContextMenu)),
                KeyCode::Esc | KeyCode::Char('q') => self.hide(),
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select_next(self.choices().len() + 3),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char(' ') | KeyCode::Enter => return Ok(self.activate_category_row()),
            KeyCode::Char('a') => return Ok(Some(self.set_all())),
            KeyCode::Char('x') => return Ok(Some(self.set_none())),
            KeyCode::Char('i') => return Ok(Some(self.invert())),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                self.category = None;
                self.state.select(Some(0));
            }
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
        if self.category.is_none() {
            if index >= 5 {
                return Ok(None);
            }
            self.state.select(Some(index));
            if index == 4 {
                self.filters = GraphFilters::default();
                self.first_parent = false;
                return Ok(Some(Action::ResetGraphFilters));
            }
            self.category = Some(match index {
                1 => Category::Authors,
                2 => Category::Refs,
                3 => Category::Views,
                _ => Category::Branches,
            });
            self.state.select(Some(0));
            return Ok(None);
        }
        if index < self.choices().len() + 3 {
            self.state.select(Some(index));
            return Ok(self.activate_category_row());
        }
        Ok(None)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }
        let t = &self.theme.overlay;
        let (title, items, height) = if let Some(category) = self.category {
            let choices = self.choices();
            let category_name = match category {
                Category::Branches => "Branches",
                Category::Authors => "Authors",
                Category::Refs => "Refs",
                Category::Views => "Views",
            };
            let mut items = vec![
                ListItem::new(" ├─ All "),
                ListItem::new(" ├─ None "),
                ListItem::new(" ├─ Invert "),
            ];
            if choices.is_empty() {
                items.push(ListItem::new(" └─ No values in this graph "));
            } else {
                items.extend(choices.iter().enumerate().map(|(index, choice)| {
                    let enabled = match category {
                        Category::Branches => self
                            .filters
                            .branches
                            .as_ref()
                            .is_none_or(|values| values.contains(choice)),
                        Category::Authors => self
                            .filters
                            .authors
                            .as_ref()
                            .is_none_or(|values| values.contains(choice)),
                        Category::Refs => match index {
                            0 => self.filters.refs.local,
                            1 => self.filters.refs.remote,
                            2 => self.filters.refs.tags,
                            _ => self.filters.refs.stashes,
                        },
                        Category::Views => self.first_parent,
                    };
                    let marker = if enabled { "x" } else { " " };
                    let connector = if index + 1 == choices.len() {
                        "└─"
                    } else {
                        "├─"
                    };
                    ListItem::new(Line::from(Span::raw(format!(
                        " {connector} [{marker}] {choice}"
                    ))))
                }));
            }
            (
                format!(" Graph filters / {category_name} "),
                items,
                choices.len().max(1) as u16 + 5,
            )
        } else {
            let items = vec![
                ListItem::new(format!(
                    " ├─ Branches ({}) ",
                    Self::selection_summary(self.filters.branches.as_ref(), self.branches.len())
                )),
                ListItem::new(format!(
                    " ├─ Authors ({}) ",
                    Self::selection_summary(self.filters.authors.as_ref(), self.authors.len())
                )),
                ListItem::new(format!(
                    " ├─ Refs ({}/4) ",
                    [
                        self.filters.refs.local,
                        self.filters.refs.remote,
                        self.filters.refs.tags,
                        self.filters.refs.stashes
                    ]
                    .into_iter()
                    .filter(|enabled| *enabled)
                    .count()
                )),
                ListItem::new(format!(
                    " ├─ Views ({}) ",
                    if self.first_parent {
                        "1 active"
                    } else {
                        "default"
                    }
                )),
                ListItem::new(" └─ Reset filters "),
            ];
            (" Graph filters ".to_string(), items, 7)
        };
        let height = height.min(area.height.saturating_sub(4)).max(5);
        let width = 52u16.min(area.width.saturating_sub(4));
        let [vertical] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vertical);
        self.rendered_area = rect;

        let hint = if self.category.is_some() {
            " ↑↓ move · Space toggle · a all · x none · i invert · ← back "
        } else {
            " ↑↓ move · → enter · r reset · ← close "
        };
        frame.render_widget(Clear, rect);
        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!("{title}—{hint}"))
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

    fn picker() -> GraphFilterPicker {
        GraphFilterPicker::new(Arc::new(Theme::default()))
    }

    #[test]
    fn branch_toggle_emits_updated_filters() {
        let mut picker = picker();
        picker.show(
            GraphFilters::default(),
            vec!["main".to_string(), "feature".to_string()],
            Vec::new(),
            false,
        );
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Enter))
            .unwrap();
        for _ in 0..3 {
            picker
                .handle_key_event(KeyEvent::from(KeyCode::Down))
                .unwrap();
        }

        let action = picker
            .handle_key_event(KeyEvent::from(KeyCode::Char(' ')))
            .unwrap();

        let Some(Action::SetGraphFilters(filters)) = action else {
            panic!("expected graph filters action");
        };
        assert_eq!(
            filters.branches,
            Some(["feature".to_string()].into_iter().collect())
        );
    }

    #[test]
    fn category_none_selects_no_values() {
        let mut picker = picker();
        picker.show(
            GraphFilters::default(),
            vec!["main".to_string()],
            Vec::new(),
            false,
        );
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Enter))
            .unwrap();

        let action = picker
            .handle_key_event(KeyEvent::from(KeyCode::Char('x')))
            .unwrap();

        let Some(Action::SetGraphFilters(filters)) = action else {
            panic!("expected graph filters action");
        };
        assert_eq!(filters.branches, Some(BTreeSet::new()));
    }

    #[test]
    fn arrow_keys_enter_and_exit_a_category() {
        let mut picker = picker();
        picker.show(
            GraphFilters::default(),
            vec!["main".to_string()],
            Vec::new(),
            false,
        );

        picker
            .handle_key_event(KeyEvent::from(KeyCode::Right))
            .unwrap();
        assert!(picker.category.is_some());

        picker
            .handle_key_event(KeyEvent::from(KeyCode::Left))
            .unwrap();
        assert!(picker.category.is_none());
        assert!(picker.visible);
    }

    #[test]
    fn mouse_click_enters_a_category_and_toggles_a_value() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut picker = picker();
        picker.show(
            GraphFilters::default(),
            vec!["main".to_string()],
            Vec::new(),
            false,
        );
        picker.rendered_area = Rect::new(0, 0, 40, 12);

        assert!(
            picker
                .handle_mouse_event(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 1,
                    modifiers: KeyModifiers::NONE,
                })
                .unwrap()
                .is_none()
        );
        let action = picker
            .handle_mouse_event(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();

        assert!(
            matches!(action, Some(Action::SetGraphFilters(filters)) if filters.branches == Some(BTreeSet::new()))
        );
    }
}
