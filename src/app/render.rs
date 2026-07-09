use super::*;

impl App {
    pub(super) fn clear_expired_messages(&mut self) -> bool {
        let had_error = self.error_message.is_some();
        let had_success = self.success_message.is_some();

        if let Some((_, when)) = &self.error_message
            && when.elapsed().as_secs() >= 5
        {
            self.error_message = None;
        }
        if let Some((_, when)) = &self.success_message
            && when.elapsed().as_secs() >= 3
        {
            self.success_message = None;
        }

        had_error != self.error_message.is_some() || had_success != self.success_message.is_some()
    }
    pub(super) fn draw(&mut self, frame: &mut ratatui::Frame) -> Result<()> {
        let area = frame.area();

        // Vertical: main area + status bar
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let main_area = outer[0];
        let status_area = outer[1];

        // Three-panel layout (plus an optional 4th GitHub panel). Every border
        // drags to resize in both orientations.
        self.horizontal_layout = main_area.width >= 100;
        let show_github = self.show_github_panel();
        self.github_visible = show_github;
        // Never leave focus on a panel that isn't drawn this frame.
        if !show_github && self.focus == FocusPanel::GitHub {
            self.focus = FocusPanel::Graph;
        }

        // Keep the graph/github split right of the changes/graph split so the
        // panel never inverts if the first two borders were dragged while the
        // GitHub panel was hidden.
        if show_github {
            let axis = if self.horizontal_layout {
                main_area.width
            } else {
                main_area.height
            };
            let min_gap = 3.0 / (axis.max(1) as f64);
            self.border_frac[2] =
                self.border_frac[2].clamp(self.border_frac[1] + min_gap, 1.0 - min_gap);
        }

        let (repo_area, changes_area, graph_area, github_area) = if self.horizontal_layout {
            let w = main_area.width as f64;
            let c1 = (self.border_frac[0] * w).round() as u16;
            let c2 = ((self.border_frac[1] - self.border_frac[0]) * w).round() as u16;
            if show_github {
                // graph is fraction-sized; github takes the remainder, so
                // dragging the graph/github border resizes them together (as the
                // changes/graph border does for changes + graph).
                let c3 = ((self.border_frac[2] - self.border_frac[1]) * w).round() as u16;
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(c1),
                        Constraint::Length(c2),
                        Constraint::Length(c3),
                        Constraint::Min(8),
                    ])
                    .split(main_area);
                (chunks[0], chunks[1], chunks[2], chunks[3])
            } else {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(c1),
                        Constraint::Length(c2),
                        Constraint::Min(8),
                    ])
                    .split(main_area);
                (chunks[0], chunks[1], chunks[2], Rect::default())
            }
        } else {
            let h = main_area.height as f64;
            let r1 = (self.border_frac[0] * h).round() as u16;
            let r2 = ((self.border_frac[1] - self.border_frac[0]) * h).round() as u16;
            if show_github {
                let r3 = ((self.border_frac[2] - self.border_frac[1]) * h).round() as u16;
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(r1),
                        Constraint::Length(r2),
                        Constraint::Length(r3),
                        Constraint::Min(3),
                    ])
                    .split(main_area);
                (chunks[0], chunks[1], chunks[2], chunks[3])
            } else {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(r1),
                        Constraint::Length(r2),
                        Constraint::Min(3),
                    ])
                    .split(main_area);
                (chunks[0], chunks[1], chunks[2], Rect::default())
            }
        };

        self.repo_area = repo_area;
        self.changes_area = changes_area;
        self.graph_area = graph_area;
        self.github_area = github_area;

        self.repo_list.focused = self.focus == FocusPanel::Repos;
        self.file_list.focused = self.focus == FocusPanel::Changes;
        self.git_graph.focused = self.focus == FocusPanel::Graph;
        self.github_panel.focused = self.focus == FocusPanel::GitHub;

        self.file_list.horizontal_layout = self.horizontal_layout;
        self.git_graph.horizontal_layout = self.horizontal_layout;
        self.github_panel.horizontal_layout = self.horizontal_layout;

        // Start each render from a blank buffer. Split panels can collapse,
        // logs may have written into the terminal before raw mode, and shorter
        // list/detail content must overwrite old longer content deterministically.
        frame.buffer_mut().reset();

        self.repo_list.draw(frame, repo_area)?;
        self.file_list.draw(frame, changes_area)?;
        self.git_graph.draw(frame, graph_area)?;
        if show_github {
            self.github_panel.draw(frame, github_area)?;
        }

        // Paint thick seam borders in horizontal mode to signal "draggable".
        // Vertical mode doesn't need this — the full-width horizontal borders
        // are already easy grab targets, and painting over them destroys titles.
        if self.horizontal_layout {
            use ratatui::style::Style;

            let buf = frame.buffer_mut();
            let mut seams = vec![
                (
                    self.dragging_border == Some(0),
                    repo_area.x + repo_area.width.saturating_sub(1),
                    changes_area.x,
                ),
                (
                    self.dragging_border == Some(1),
                    changes_area.x + changes_area.width.saturating_sub(1),
                    graph_area.x,
                ),
            ];
            if show_github {
                seams.push((
                    self.dragging_border == Some(2),
                    graph_area.x + graph_area.width.saturating_sub(1),
                    github_area.x,
                ));
            }
            for (dragging, x_a, x_b) in seams {
                let color = if dragging {
                    self.theme.overlay.border_drag_active
                } else {
                    self.theme.overlay.border_drag_idle
                };
                let style = Style::default().fg(color);
                for x in [x_a, x_b] {
                    for y in repo_area.y..repo_area.y + repo_area.height {
                        if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                            cell.set_symbol("█");
                            cell.set_style(style);
                        }
                    }
                }
            }
        } else if self.dragging_border.is_some() {
            use ratatui::style::Style;
            let style = Style::default().fg(self.theme.overlay.border_drag_active);
            let buf = frame.buffer_mut();
            let mut seams = vec![
                (self.dragging_border == Some(0), changes_area.y),
                (self.dragging_border == Some(1), graph_area.y),
            ];
            if show_github {
                seams.push((self.dragging_border == Some(2), github_area.y));
            }
            for (dragging, y) in seams {
                if !dragging {
                    continue;
                }
                // Paint just the border characters (skip first col = title area preserved)
                for x in repo_area.x..repo_area.x + repo_area.width {
                    if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                        cell.set_style(style);
                    }
                }
            }
        }

        self.clear_expired_messages();

        self.status_bar.focus = self.focus;
        self.status_bar.sort_order = self.sort_order;
        self.status_bar.error = self.error_message.clone();
        self.status_bar.success = self.success_message.clone();
        self.status_bar.draw(frame, status_area)?;

        // Overlays rendered last
        self.context_menu.draw(frame, area)?;
        self.path_input.draw(frame, area);
        self.confirm_dialog.draw(frame, area);
        self.theme_picker.draw(frame, area);
        self.picker.draw(frame, area);

        // Update notification overlay
        if let Some(ref version) = self.update_version {
            self.draw_update_notification(frame, main_area, version);
        }

        // Help overlay (rendered last so it's on top of everything)
        if self.show_help {
            self.draw_help(frame, main_area);
        }

        Ok(())
    }
    pub(super) fn draw_update_notification(
        &self,
        frame: &mut ratatui::Frame,
        area: Rect,
        version: &str,
    ) {
        use ratatui::style::Style;
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let t = &self.theme.overlay;
        let text = format!(" \u{2191} v{version} \u{00b7} gitpane update ");
        let width = text.len() as u16 + 2;
        let height = 3;

        if area.width < width || area.height < height {
            return;
        }

        let x = match self.update_position {
            UpdatePosition::TopRight => area.x + area.width.saturating_sub(width + 1),
            UpdatePosition::TopLeft => area.x + 1,
        };
        let y = area.y;

        let rect = Rect::new(x, y, width, height);

        let line = Line::from(vec![
            Span::styled(" \u{2191} ", Style::default().fg(t.update_toast_arrow)),
            Span::styled(
                format!("v{version}"),
                Style::default().fg(t.update_toast_version),
            ),
            Span::styled(
                " \u{00b7} gitpane update ",
                Style::default().fg(t.update_toast_install),
            ),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.update_toast_border));

        let paragraph = Paragraph::new(line).block(block);

        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(paragraph, rect);
    }
    pub(super) fn draw_help(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let t = &self.theme.overlay;
        let key = |k: &str| Span::styled(format!("  {k:<10}"), Style::default().fg(t.help_key));
        let desc = |d: &str| Span::raw(d.to_string());
        let section = |title: &str| {
            Line::from(Span::styled(
                format!(" {title}"),
                Style::default().add_modifier(Modifier::BOLD),
            ))
        };

        let mut lines = vec![
            section("Global"),
            Line::from(vec![key("?"), desc("Toggle this help")]),
            Line::from(vec![key("Tab"), desc("Cycle focus forward")]),
            Line::from(vec![key("Shift+Tab"), desc("Cycle focus backward")]),
            Line::from(vec![key("Esc"), desc("Close / go back")]),
            Line::from(vec![key("r"), desc("Refresh all repos")]),
            Line::from(vec![key("o"), desc("Open repo/worktree")]),
            Line::from(vec![key("v"), desc("Review changes (tmux window)")]),
            Line::from(vec![key("G"), desc("Attach live tmux session")]),
            Line::from(vec![key("p"), desc("Toggle GitHub panel")]),
            Line::from(vec![key("y"), desc("Copy to clipboard")]),
            Line::from(vec![key("q"), desc("Quit")]),
        ];

        match self.focus {
            FocusPanel::Repos => {
                lines.push(Line::from(""));
                lines.push(section("Repos"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("a"), desc("Add repo")]));
                lines.push(Line::from(vec![
                    key("d"),
                    desc("Remove repo / worktree (confirm)"),
                ]));
                lines.push(Line::from(vec![key("s"), desc("Cycle sort order")]));
                lines.push(Line::from(vec![key("w"), desc("Toggle worktrees")]));
                lines.push(Line::from(vec![
                    key("right-click"),
                    desc("Menu: new worktree, push/pull, …"),
                ]));
                lines.push(Line::from(vec![key("R"), desc("Rescan repos")]));
                lines.push(Line::from(vec![key("g"), desc("Open git graph")]));
            }
            FocusPanel::Changes => {
                lines.push(Line::from(""));
                lines.push(section("Changes"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("Enter"), desc("Open diff view")]));
                lines.push(Line::from(vec![key("Esc / h"), desc("Close diff view")]));
            }
            FocusPanel::Graph => {
                lines.push(Line::from(""));
                lines.push(section("Graph"));
                lines.push(Line::from(vec![key("j / k"), desc("Move up / down")]));
                lines.push(Line::from(vec![key("h / l"), desc("Scroll left / right")]));
                lines.push(Line::from(vec![key("Enter"), desc("Open commit files")]));
                lines.push(Line::from(""));
                lines.push(section("Search"));
                lines.push(Line::from(vec![key("/"), desc("Search commits")]));
                lines.push(Line::from(vec![key("n / N"), desc("Next / prev match")]));
                lines.push(Line::from(""));
                lines.push(section("View"));
                lines.push(Line::from(vec![key("f"), desc("First-parent mode")]));
                lines.push(Line::from(vec![key("c"), desc("Collapse / expand branch")]));
                lines.push(Line::from(vec![key("H"), desc("Expand all collapsed")]));
            }
            FocusPanel::GitHub => {
                lines.push(Line::from(""));
                lines.push(section("GitHub"));
                lines.push(Line::from(vec![key("j / k"), desc("Move / scroll")]));
                lines.push(Line::from(vec![
                    key("Enter"),
                    desc("Preview, then open in browser"),
                ]));
                lines.push(Line::from(vec![key("Esc"), desc("Close preview")]));
                lines.push(Line::from(vec![
                    key("c"),
                    desc("Cycle open / all / closed"),
                ]));
                lines.push(Line::from(vec![key("p"), desc("Hide panel")]));
            }
        }

        let height = (lines.len() as u16 + 2).min(area.height);
        let width = 42u16.min(area.width);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let help_area = Rect::new(x, y, width, height);

        let panel_name = match self.focus {
            FocusPanel::Repos => "Repos",
            FocusPanel::Changes => "Changes",
            FocusPanel::Graph => "Graph",
            FocusPanel::GitHub => "GitHub",
        };
        let block = Block::default()
            .title(format!(" Keybindings \u{2014} {panel_name} "))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.help_border))
            .style(Style::default().bg(t.help_bg));

        frame.render_widget(ratatui::widgets::Clear, help_area);
        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, help_area);
    }
}
