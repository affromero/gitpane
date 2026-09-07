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
use crate::components::ListFilter;
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
    /// Substring filter over the current category's choices (`/` to type).
    filter: ListFilter,
    /// True while the picker collects filter characters (`/` pressed).
    searching: bool,
    /// Choices of the currently open category, cached (once per category
    /// entry) so keypresses and draws don't re-clone the branch/author list.
    choices_cache: Vec<String>,
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
            filter: ListFilter::default(),
            searching: false,
            choices_cache: Vec::new(),
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    /// Whether the picker is inside a filter category (branches/authors/refs
    /// views); only then does the status-bar hint apply.
    pub fn is_filtering(&self) -> bool {
        self.category.is_some()
    }

    /// Key hint for the bottom status bar while a filter category is open.
    /// `None` on the root screen / when nothing is filterable: the repo
    /// status legend stays.
    pub fn hint_text(&self) -> Option<String> {
        match self.category? {
            Category::Branches | Category::Authors => Some(self.hint_for_list().to_string()),
            Category::Refs => Some(self.hint_for_list().to_string()),
            Category::Views => Some(
                if self.searching {
                    " type to filter · ↑↓ move · Space toggle · Esc clear · ← back "
                } else {
                    " ↑↓ move · Space toggle · ← back "
                }
                .to_string(),
            ),
        }
    }

    fn hint_for_list(&self) -> &'static str {
        if self.searching {
            " type to filter · ↑↓ move · Space toggle · Esc clear · ← back "
        } else {
            " ↑↓ move · Space toggle · / filter · a all · x none · i invert · ← back "
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.category = None;
        self.state.select(None);
        self.filter.clear();
        self.choices_cache.clear();
        self.searching = false;
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
        self.filter.clear();
        self.choices_cache.clear();
        self.searching = false;
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

    /// Number of rows in the value list: 3 action rows plus the (filtered)
    /// choices. While the filter is active, only matching choices are shown.
    fn visible_count(&self) -> usize {
        let n = if self.filter.is_active() {
            self.filter.count() + 3
        } else {
            self.choices_cache.len() + 3
        };
        n.max(3)
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
        let choices = &self.choices_cache;
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
        let all: BTreeSet<String> = choices.iter().cloned().collect();
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
            index => self.toggle_choice(self.filter.visible_at(index - 3)?),
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
        let all: BTreeSet<String> = self.choices_cache.iter().cloned().collect();
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
                    self.choices_cache = self.choices();
                    self.filter.clear();
                    self.filter.rebuild(&self.choices_cache);
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

        if self.searching {
            match key.code {
                // Space/Enter first: the generic `Char(c)` arm below would
                // swallow `' '` as a query character.
                KeyCode::Char(' ') | KeyCode::Enter => return Ok(self.activate_category_row()),
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.filter.rebuild(&self.choices_cache);
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.filter.rebuild(&self.choices_cache);
                }
                KeyCode::Esc => {
                    self.searching = false;
                    self.filter.clear();
                    self.filter.rebuild(&self.choices_cache);
                }
                KeyCode::Up => self.select_prev(),
                KeyCode::Down => self.select_next(self.visible_count()),
                KeyCode::Left => {
                    self.searching = false;
                    self.filter.clear();
                    self.filter.rebuild(&self.choices_cache);
                    self.category = None;
                    self.state.select(Some(0));
                }
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('/') => {
                self.searching = true;
            }
            KeyCode::Char('j') | KeyCode::Down => self.select_next(self.visible_count()),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char(' ') | KeyCode::Enter => return Ok(self.activate_category_row()),
            KeyCode::Char('a') => return Ok(Some(self.set_all())),
            KeyCode::Char('x') => return Ok(Some(self.set_none())),
            KeyCode::Char('i') => return Ok(Some(self.invert())),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                self.searching = false;
                self.filter.clear();
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
            self.choices_cache = self.choices();
            self.filter.rebuild(&self.choices_cache);
            self.state.select(Some(0));
            return Ok(None);
        }
        if index < self.visible_count() {
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
        let choices = &self.choices_cache;
        let (title, items, height, longest) = if let Some(category) = self.category {
            let category_name = match category {
                Category::Branches => "Branches",
                Category::Authors => "Authors",
                Category::Refs => "Refs",
                Category::Views => "Views",
            };
            let visible_n = if self.filter.is_active() {
                self.filter.count()
            } else {
                choices.len()
            };
            let longest = (0..visible_n)
                .filter_map(|i| self.filter.visible_at(i))
                .map(|i| choices[i].chars().count())
                .max()
                .unwrap_or(0);
            let mut items: Vec<ListItem<'_>> = vec![
                ListItem::new(" ├─ All "),
                ListItem::new(" ├─ None "),
                ListItem::new(" ├─ Invert "),
            ];
            if visible_n == 0 {
                items.push(ListItem::new(" └─ No matches "));
            } else {
                for fpos in 0..visible_n {
                    let Some(real) = self.filter.visible_at(fpos) else {
                        continue;
                    };
                    let choice = &choices[real];
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
                        Category::Refs => match real {
                            0 => self.filters.refs.local,
                            1 => self.filters.refs.remote,
                            2 => self.filters.refs.tags,
                            _ => self.filters.refs.stashes,
                        },
                        Category::Views => self.first_parent,
                    };
                    let marker = if enabled { "x" } else { " " };
                    let connector = if fpos + 1 == visible_n {
                        "└─"
                    } else {
                        "├─"
                    };
                    let mut spans = vec![Span::raw(format!(" {connector} [{marker}] "))];
                    spans.extend(crate::components::highlight_matches(
                        choice,
                        self.filter.query(),
                        Style::default(),
                        Style::default()
                            .fg(t.path_input_prompt)
                            .add_modifier(Modifier::BOLD),
                    ));
                    items.push(ListItem::new(Line::from(spans)));
                }
            }
            let query_tail = if self.searching || self.filter.is_active() {
                // The caret makes an empty query visible as "typing mode".
                format!(" — /{}▏ ", self.filter.query())
            } else {
                String::new()
            };
            (
                format!(" Graph filters / {category_name}{query_tail}"),
                items,
                visible_n.max(1) as u16 + 5,
                longest,
            )
        } else {
            let longest = 0;
            let items: Vec<ListItem<'_>> = vec![
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
            (" Graph filters ".to_string(), items, 7, longest)
        };
        let height = height.min(area.height.saturating_sub(4)).max(5);
        // Width follows the longest visible choice so long branch/author names
        // aren't truncated by a fixed width, clamped to the terminal.
        let width = (52u16.max(longest as u16 + 12)).min(area.width.saturating_sub(4));
        let [vertical] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vertical);
        self.rendered_area = rect;

        frame.render_widget(Clear, rect);
        let list = List::new(items)
            .block(
                Block::default()
                    .title(title)
                    .title_style(if self.searching || self.filter.is_active() {
                        Style::default().fg(t.path_input_prompt)
                    } else {
                        Style::default()
                    })
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.context_menu_border)),
            )
            .highlight_style(
                Style::default()
                    .bg(t.context_menu_selection_bg)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, rect, &mut self.state);
        self.rendered_area = rect;
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

#[cfg(test)]
mod search_tests {
    use super::*;

    fn new_picker() -> GraphFilterPicker {
        GraphFilterPicker::new(Arc::new(Theme::default()))
    }

    #[test]
    fn search_narrows_then_toggles_a_matching_branch() {
        let mut picker = new_picker();
        picker.show(
            GraphFilters::default(),
            vec![
                "main".to_string(),
                "feature/x".to_string(),
                "feature/y".to_string(),
            ],
            Vec::new(),
            false,
        );
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Enter))
            .unwrap();
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Char('/')))
            .unwrap();
        for c in "feat".chars() {
            picker
                .handle_key_event(KeyEvent::from(KeyCode::Char(c)))
                .unwrap();
        }
        // 3 action rows + 2 matches; down to the first match and toggle it.
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
        // filters.branches was None (= all); toggling feature/x unselects it.
        let expected: BTreeSet<String> = ["main", "feature/y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(filters.branches, Some(expected));
    }

    #[test]
    fn esc_clears_search_before_leaving_category() {
        let mut picker = new_picker();
        picker.show(
            GraphFilters::default(),
            vec!["alpha".to_string(), "beta".to_string()],
            Vec::new(),
            false,
        );
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Enter))
            .unwrap();
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Char('/')))
            .unwrap();
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Char('b')))
            .unwrap();
        assert_eq!(picker.filter.count(), 1);
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Esc))
            .unwrap();
        assert!(!picker.filter.is_active());
        assert!(
            picker.category.is_some(),
            "first Esc only clears the filter"
        );
        picker
            .handle_key_event(KeyEvent::from(KeyCode::Esc))
            .unwrap();
        assert!(picker.category.is_none());
    }
}
