//! Commit search: the `/` overlay's input handling and the match list it walks.
//!
//! Split out of `mod.rs` to keep the graph module under the 1000-line file cap;
//! the state itself lives on `GitGraph` (`search`), so these stay `impl GitGraph`
//! methods and see the same private fields the rest of the graph code does.

use super::*;

impl GitGraph {
    pub fn search_visible(&self) -> bool {
        self.search.visible
    }

    pub fn handle_search_key(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Esc => {
                self.search.visible = false;
            }
            KeyCode::Enter => {
                self.search.visible = false;
                // Jump to first match if any
                if let Some(&idx) = self.search.matches.first() {
                    self.search.current_match = Some(0);
                    self.state.select(Some(idx));
                }
            }
            KeyCode::Backspace => {
                self.search.input.pop();
                self.update_search_matches();
            }
            KeyCode::Char(c) => {
                self.search.input.push(c);
                self.update_search_matches();
            }
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn update_search_matches(&mut self) {
        self.search.current_match = None;
        if self.search.input.is_empty() {
            self.search.matches.clear();
            return;
        }
        let query = self.search.input.to_lowercase();
        let matches: Vec<usize> = self
            .display_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.message.to_lowercase().contains(&query)
                    || row.author.to_lowercase().contains(&query)
                    || row.short_id.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if !matches.is_empty() {
            self.search.current_match = Some(0);
        }
        self.search.matches = matches;
    }

    pub(super) fn search_next(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let next = match self.search.current_match {
            Some(i) => (i + 1) % self.search.matches.len(),
            None => 0,
        };
        self.search.current_match = Some(next);
        self.state.select(Some(self.search.matches[next]));
    }

    pub(super) fn search_prev(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let prev = match self.search.current_match {
            Some(0) | None => self.search.matches.len() - 1,
            Some(i) => i - 1,
        };
        self.search.current_match = Some(prev);
        self.state.select(Some(self.search.matches[prev]));
    }
}
