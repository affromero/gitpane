use super::*;

impl App {
    /// Index of the first user keybinding whose `key` is exactly the character
    /// `c`, or `None`. Keys are single characters; a multi-char `key` never
    /// matches (so it is inert rather than an error).
    fn custom_keybinding_index(&self, c: char) -> Option<usize> {
        self.config
            .keybindings
            .iter()
            .position(|kb| crate::config::key_matches(&kb.key, c))
    }

    pub(super) fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        // Ctrl+C always quits, before any overlay or focus-specific routing.
        // Raw mode clears ISIG, so the terminal never raises SIGINT for Ctrl+C
        // — this key binding is the only way it can terminate the app.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.action_tx.send(Action::Quit)?;
            return Ok(());
        }

        // Confirm dialog gets top priority
        if self.confirm_dialog.visible {
            if let Some(action) = self.confirm_dialog.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Path input gets priority
        if self.path_input.visible {
            if let Some(action) = self.path_input.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Theme picker gets priority once visible; blocks the global `t` /
        // help / focus-routed keys until the user commits or cancels.
        if self.theme_picker.visible {
            if let Some(action) = self.theme_picker.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        if self.picker.visible {
            if let Some(action) = self.picker.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Search input gets priority when active
        if self.focus == FocusPanel::Graph && self.git_graph.search_visible() {
            self.git_graph.handle_search_key(key)?;
            return Ok(());
        }

        // Context menu is modal: it consumes every key while open (an unknown key
        // dismisses it) so e.g. `q`/`d` can't both close the menu AND fire the
        // global quit/delete action.
        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_key_event(key)? {
                self.action_tx.send(action)?;
            }
            return Ok(());
        }

        // Help overlay: ? toggles, any other key dismisses
        if key.code == KeyCode::Char('?') {
            self.show_help = !self.show_help;
            return Ok(());
        } else if self.show_help {
            self.show_help = false;
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                // If viewing a diff or the GitHub detail, close it instead of quitting.
                if self.focus == FocusPanel::Changes && self.file_list.viewing_diff() {
                    self.file_list.handle_key_event(key)?;
                    return Ok(());
                }
                if self.focus == FocusPanel::GitHub && self.github_panel.has_detail() {
                    self.github_panel.handle_key_event(key)?;
                    return Ok(());
                }
                self.action_tx.send(Action::Quit)?;
            }
            KeyCode::Esc => {
                // Close active detail/diff first, then navigate panels
                if self.focus == FocusPanel::Changes && self.file_list.viewing_diff() {
                    self.file_list.handle_key_event(key)?;
                } else if self.focus == FocusPanel::Graph && self.git_graph.has_detail() {
                    self.git_graph.handle_key_event(key)?;
                } else if self.focus == FocusPanel::GitHub && self.github_panel.has_detail() {
                    self.github_panel.handle_key_event(key)?;
                } else {
                    match self.focus {
                        FocusPanel::GitHub => self.focus = FocusPanel::Graph,
                        FocusPanel::Graph => self.focus = FocusPanel::Changes,
                        FocusPanel::Changes => self.focus = FocusPanel::Repos,
                        FocusPanel::Repos => self.action_tx.send(Action::Quit)?,
                    }
                }
            }
            KeyCode::Tab => {
                // Cycle focus right, skipping the GitHub panel when it's hidden.
                self.focus = match self.focus {
                    FocusPanel::Repos => FocusPanel::Changes,
                    FocusPanel::Changes => FocusPanel::Graph,
                    FocusPanel::Graph if self.github_visible => FocusPanel::GitHub,
                    FocusPanel::Graph => FocusPanel::Repos,
                    FocusPanel::GitHub => FocusPanel::Repos,
                };
            }
            KeyCode::BackTab => {
                // Cycle focus left, skipping the GitHub panel when it's hidden.
                self.focus = match self.focus {
                    FocusPanel::Repos if self.github_visible => FocusPanel::GitHub,
                    FocusPanel::Repos => FocusPanel::Graph,
                    FocusPanel::Changes => FocusPanel::Repos,
                    FocusPanel::Graph => FocusPanel::Changes,
                    FocusPanel::GitHub => FocusPanel::Graph,
                };
            }
            KeyCode::Char('r') => {
                self.action_tx.send(Action::RefreshAll)?;
            }
            KeyCode::Char('t') => {
                self.action_tx.send(Action::OpenThemePicker)?;
            }
            KeyCode::Char('R') => {
                self.action_tx.send(Action::RescanRepos)?;
            }
            KeyCode::Char('g') => {
                self.action_tx.send(Action::ShowGitGraph)?;
            }
            KeyCode::Char('p') => {
                self.toggle_github_panel();
            }
            KeyCode::Char('G') => {
                self.action_tx.send(Action::GotoSessionSelected)?;
            }
            KeyCode::Char('o') => {
                self.action_tx.send(Action::OpenSelected)?;
            }
            KeyCode::Char('v') => {
                self.action_tx.send(Action::ReviewSelected)?;
            }
            KeyCode::Char('a') => {
                self.action_tx.send(Action::OpenAddRepo)?;
            }
            KeyCode::Char('d') => {
                // On a worktree row, `d` removes that worktree; on a repo row it
                // removes the repo. Both go through a confirmation.
                if self.repo_list.selected_worktree().is_some() {
                    self.action_tx.send(Action::RemoveWorktreeSelected)?;
                } else if let Some(idx) = self.repo_list.selected_index() {
                    let entry = &self.repo_list.repos[idx];
                    let name = entry.name.clone();
                    let repo_id = RepoId(entry.path.clone());
                    self.confirm_dialog
                        .show(format!("Remove {}?", name), Action::RemoveRepo(repo_id));
                }
            }
            KeyCode::Char('s') => {
                self.action_tx.send(Action::CycleSortOrder)?;
            }
            KeyCode::Char('y') => {
                // Copy selected item to clipboard (OSC 52)
                let text = match self.focus {
                    FocusPanel::Repos => self
                        .repo_list
                        .selected_repo()
                        .map(|e| e.path.to_string_lossy().to_string()),
                    FocusPanel::Changes => self.file_list.selected_path(),
                    FocusPanel::Graph => self.git_graph.selected_text(),
                    FocusPanel::GitHub => self.github_panel.selected_url(),
                };
                if let Some(text) = text {
                    use std::io::Write;
                    let encoded = base64_encode(text.as_bytes());
                    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                    let _ = std::io::stdout().flush();
                }
            }
            _ => {
                // A user-defined keybinding (config `[[keybindings]]`) fires
                // before panel routing, so it can shadow a panel-local key. It
                // never reaches keys claimed by the built-in globals above,
                // which are matched first and return — those stay reserved.
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && let Some(idx) = self.custom_keybinding_index(c)
                {
                    self.action_tx.send(Action::RunKeybinding(idx))?;
                    return Ok(());
                }
                // Route to focused panel
                match self.focus {
                    FocusPanel::Repos => {
                        if let Some(action) = self.repo_list.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                    FocusPanel::Changes => {
                        if let Some(action) = self.file_list.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                    FocusPanel::Graph => {
                        if let Some(action) = self.git_graph.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                    FocusPanel::GitHub => {
                        if let Some(action) = self.github_panel.handle_key_event(key)? {
                            self.action_tx.send(action)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
        use crossterm::event::{MouseButton, MouseEventKind};

        // Modal overlays swallow mouse input so a click can't leak through to a
        // panel or open a context menu hidden behind them.
        if self.picker.visible
            || self.theme_picker.visible
            || self.path_input.visible
            || self.confirm_dialog.visible
        {
            return Ok(());
        }

        if self.context_menu.visible {
            if let Some(action) = self.context_menu.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            } else if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.context_menu.hide();
            }
            return Ok(());
        }

        let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
        const GRAB_ZONE: u16 = 2; // ±2 cells hit zone for border grab

        // Border dragging for panel resize (works in both orientations)
        if self.repo_area.width > 0 {
            // Compute border positions and mouse coordinate along the layout
            // axis. border3 (graph|github) only exists while the panel is shown.
            let (border1, border2, border3, mouse_pos, total, origin) = if self.horizontal_layout {
                (
                    self.repo_area.x + self.repo_area.width,
                    self.changes_area.x + self.changes_area.width,
                    self.graph_area.x + self.graph_area.width,
                    mouse.column,
                    self.repo_area.width
                        + self.changes_area.width
                        + self.graph_area.width
                        + self.github_area.width,
                    self.repo_area.x,
                )
            } else {
                (
                    self.repo_area.y + self.repo_area.height,
                    self.changes_area.y + self.changes_area.height,
                    self.graph_area.y + self.graph_area.height,
                    mouse.row,
                    self.repo_area.height
                        + self.changes_area.height
                        + self.graph_area.height
                        + self.github_area.height,
                    self.repo_area.y,
                )
            };

            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Grab the nearest border within the hit zone.
                    let mut candidates = vec![
                        (0u8, mouse_pos.abs_diff(border1)),
                        (1, mouse_pos.abs_diff(border2)),
                    ];
                    if self.github_visible {
                        candidates.push((2, mouse_pos.abs_diff(border3)));
                    }
                    self.dragging_border = candidates
                        .into_iter()
                        .filter(|(_, d)| *d <= GRAB_ZONE)
                        .min_by_key(|(_, d)| *d)
                        .map(|(id, _)| id);
                    // Don't return — let the click propagate to panels
                    // so items near borders remain clickable. The drag
                    // will only engage on MouseEventKind::Drag.
                }
                MouseEventKind::Drag(MouseButton::Left) if self.dragging_border.is_some() => {
                    let rel = mouse_pos.saturating_sub(origin) as f64 / total as f64;
                    let min_f = 3.0 / total as f64;
                    match self.dragging_border {
                        Some(0) => {
                            self.border_frac[0] = rel.clamp(min_f, self.border_frac[1] - min_f);
                        }
                        Some(1) => {
                            // Cap at the graph/github split when the panel is shown.
                            let upper = if self.github_visible {
                                self.border_frac[2]
                            } else {
                                1.0
                            } - min_f;
                            self.border_frac[1] = rel.clamp(self.border_frac[0] + min_f, upper);
                        }
                        Some(2) => {
                            self.border_frac[2] =
                                rel.clamp(self.border_frac[1] + min_f, 1.0 - min_f);
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) if self.dragging_border.is_some() => {
                    self.dragging_border = None;
                    return Ok(());
                }
                _ => {}
            }
        }

        // Set focus on left click based on which panel was clicked
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self.repo_area.contains(pos) {
                self.focus = FocusPanel::Repos;
            } else if self.changes_area.contains(pos) {
                self.focus = FocusPanel::Changes;
            } else if self.graph_area.contains(pos) {
                self.focus = FocusPanel::Graph;
            } else if self.github_visible && self.github_area.contains(pos) {
                self.focus = FocusPanel::GitHub;
            }
        }

        // Route to the panel under the mouse
        if self.repo_area.contains(pos) {
            if let Some(action) = self.repo_list.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            }
        } else if self.changes_area.contains(pos) {
            if let Some(action) = self.file_list.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            }
        } else if self.graph_area.contains(pos) {
            if let Some(action) = self.git_graph.handle_mouse_event(mouse)? {
                self.action_tx.send(action)?;
            }
        } else if self.github_visible
            && self.github_area.contains(pos)
            && let Some(action) = self.github_panel.handle_mouse_event(mouse)?
        {
            self.action_tx.send(action)?;
        }
        Ok(())
    }
}
