pub(crate) mod confirm_dialog;
pub(crate) mod context_menu;
pub(crate) mod file_list;
pub(crate) mod git_graph;
pub(crate) mod github_panel;
pub(crate) mod graph_menu;
pub(crate) mod path_input;
pub(crate) mod picker;
pub(crate) mod repo_list;
pub(crate) mod status_bar;
pub(crate) mod theme_picker;

use color_eyre::Result;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;

#[allow(dead_code)]
pub(crate) trait Component {
    fn register_action_handler(&mut self, _tx: UnboundedSender<Action>) -> Result<()> {
        Ok(())
    }

    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_key_event(&mut self, _key: KeyEvent) -> Result<Option<Action>> {
        Ok(None)
    }

    fn handle_mouse_event(&mut self, _mouse: MouseEvent) -> Result<Option<Action>> {
        Ok(None)
    }

    fn update(&mut self, _action: Action) -> Result<Option<Action>> {
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()>;
}

/// Case-insensitive substring filter over a list of item strings.
/// Tracks the underlying indices that match the current query so callers can
/// either render a filtered view or jump/highlight within the full list.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListFilter {
    query: String,
    /// Underlying indices matching the query (all indices when empty).
    matches: Vec<usize>,
}

impl ListFilter {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn pop(&mut self) {
        self.query.pop();
    }

    /// Rebuild `matches` against `items`. Call after any query or item change.
    pub fn rebuild(&mut self, items: &[String]) {
        let q = self.query.to_lowercase();
        self.matches = if q.is_empty() {
            (0..items.len()).collect()
        } else {
            items
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.to_lowercase().contains(&q).then_some(i))
                .collect()
        };
    }

    pub fn count(&self) -> usize {
        self.matches.len()
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// The underlying index of the n-th matching item, or `None` out of range.
    pub fn visible_at(&self, filtered_pos: usize) -> Option<usize> {
        self.matches.get(filtered_pos).copied()
    }

    /// Position (0-based) of an underlying index within the matches, if it
    /// currently matches.
    pub fn position_of(&self, idx: usize) -> Option<usize> {
        self.matches.iter().position(|&m| m == idx)
    }
}

/// Split `text` into spans with the (first) case-insensitive occurrence of
/// `query` styled as `match_style` and the rest as `base`. Works on byte
/// boundaries; for non-ASCII case folding the offsets may drift, in which
/// case the whole text is returned with the base style.
pub(crate) fn highlight_matches<'a>(
    text: &'a str,
    query: &str,
    base: Style,
    match_style: Style,
) -> Vec<Span<'a>> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return vec![Span::styled(text, base)];
    }
    let lower = text.to_lowercase();
    match lower.find(&q) {
        Some(start) => {
            let end = start + q.len();
            if end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end) {
                vec![
                    Span::styled(&text[..start], base),
                    Span::styled(&text[start..end], match_style),
                    Span::styled(&text[end..], base),
                ]
            } else {
                vec![Span::styled(text, base)]
            }
        }
        None => vec![Span::styled(text, base)],
    }
}

#[cfg(test)]
mod list_filter_tests {
    use super::ListFilter;

    #[test]
    fn matches_are_case_insensitive_substrings() {
        let items: Vec<String> = ["Hello", "world", "hello!", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut f = ListFilter::default();
        f.rebuild(&items);
        assert_eq!(f.count(), 4);
        f.push('h');
        f.push('e');
        f.rebuild(&items);
        assert_eq!(f.matches(), &[0, 2]);
        assert_eq!(f.visible_at(1), Some(2));
        assert_eq!(f.position_of(0), Some(0));
        // no matches
        f.clear();
        f.push('z');
        f.rebuild(&items);
        assert_eq!(f.count(), 0);
        assert_eq!(f.visible_at(0), None);
    }

    #[test]
    fn pop_and_clear_restore_full_list() {
        let items: Vec<String> = ["abc", "def"].iter().map(|s| s.to_string()).collect();
        let mut f = ListFilter::default();
        f.rebuild(&items);
        f.push('a');
        f.rebuild(&items);
        assert_eq!(f.count(), 1);
        f.pop();
        f.rebuild(&items);
        assert_eq!(f.count(), 2);
        f.clear();
        f.rebuild(&items);
        assert_eq!(f.count(), 2);
        assert!(!f.is_active());
    }
}
