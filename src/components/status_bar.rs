use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::sync::Arc;
use std::time::Instant;

use crate::app::{FocusPanel, SortOrder};
use crate::components::Component;
use crate::theme::{StatusBarTheme, Theme};

pub(crate) struct StatusBar {
    started_at: Instant,
    pub focus: FocusPanel,
    pub sort_order: SortOrder,
    pub error: Option<(String, Instant)>,
    pub success: Option<(String, Instant)>,
    theme: Arc<Theme>,
}

impl StatusBar {
    pub fn new(theme: Arc<Theme>) -> Self {
        Self {
            started_at: Instant::now(),
            focus: FocusPanel::Repos,
            sort_order: SortOrder::Alphabetical,
            error: None,
            success: None,
            theme,
        }
    }
}

impl Component for StatusBar {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let s = &self.theme.status_bar;
        let r = &self.theme.repo_list;
        let version_text = format!("v{} ", env!("CARGO_PKG_VERSION"));
        let version_len = version_text.len() as u16;
        let chunks =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(version_len)]).split(area);
        let content_area = chunks[0];
        let version_area = chunks[1];

        let version = Paragraph::new(version_text)
            .style(Style::default().fg(s.version))
            .right_aligned();
        frame.render_widget(version, version_area);

        if let Some((ref msg, when)) = self.error {
            if when.elapsed().as_secs() < 5 {
                let error_bar = Paragraph::new(Line::from(vec![
                    Span::styled(
                        " ERROR ",
                        Style::default()
                            .fg(s.error_label_fg)
                            .bg(s.error_label_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", msg), Style::default().fg(s.error_text)),
                ]));
                frame.render_widget(error_bar, content_area);
                return Ok(());
            } else {
                self.error = None;
            }
        }

        if let Some((ref msg, when)) = self.success {
            if when.elapsed().as_secs() < 3 {
                let success_bar = Paragraph::new(Line::from(vec![
                    Span::styled(
                        " OK ",
                        Style::default()
                            .fg(s.success_label_fg)
                            .bg(s.success_label_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", msg), Style::default().fg(s.success_text)),
                ]));
                frame.render_widget(success_bar, content_area);
                return Ok(());
            } else {
                self.success = None;
            }
        }

        let elapsed = self.started_at.elapsed().as_secs();

        let spans = if elapsed < 60 {
            vec![
                Span::styled(" * ", Style::default().fg(r.dirty_marker)),
                Span::styled("dirty ", Style::default().fg(s.legend_text)),
                Span::styled("\u{2191}", Style::default().fg(r.ahead)),
                Span::styled("push ", Style::default().fg(s.legend_text)),
                Span::styled("\u{2193}", Style::default().fg(r.behind)),
                Span::styled("pull ", Style::default().fg(s.legend_text)),
                Span::styled("\u{21e1}", Style::default().fg(r.unpushed_submodule)),
                Span::styled(" subs unpushed ", Style::default().fg(s.legend_text)),
                Span::styled("$", Style::default().fg(r.stash)),
                Span::styled(" stash ", Style::default().fg(s.legend_text)),
                Span::styled("[n]", Style::default().fg(r.file_count)),
                Span::styled(" files  ", Style::default().fg(s.legend_text)),
                dim_sep(s),
                key_span("Tab", s),
                Span::raw(" switch  "),
                key_span("Enter", s),
                Span::raw(" diff  "),
                key_span("g", s),
                Span::raw(" reload graph  "),
                key_span("r", s),
                Span::raw(" refresh  "),
                key_span("R", s),
                Span::raw(" rescan  "),
                key_span("a", s),
                Span::raw(" add  "),
                key_span("d", s),
                Span::raw(" remove  "),
                key_span("y", s),
                Span::raw(" copy  "),
                key_span("s", s),
                Span::raw(format!(" sort ({})  ", self.sort_order.label())),
                key_span("w", s),
                Span::raw(" worktrees  "),
                key_span("q", s),
                Span::raw(" quit"),
            ]
        } else {
            let focus_label = match self.focus {
                FocusPanel::Repos => "Repos",
                FocusPanel::Changes => "Changes",
                FocusPanel::Graph => "Graph",
            };
            vec![
                Span::styled(
                    format!(" {} ", focus_label),
                    Style::default()
                        .fg(s.focus_label_fg)
                        .bg(s.focus_label_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                key_span("Tab", s),
                Span::raw(" Switch  "),
                key_span("Enter", s),
                Span::raw(" Diff  "),
                key_span("g", s),
                Span::raw(" Graph  "),
                key_span("r", s),
                Span::raw(" Refresh  "),
                key_span("R", s),
                Span::raw(" Rescan  "),
                key_span("a", s),
                Span::raw(" Add  "),
                key_span("d", s),
                Span::raw(" Remove  "),
                key_span("y", s),
                Span::raw(" Copy  "),
                key_span("s", s),
                Span::raw(format!(" Sort ({})  ", self.sort_order.label())),
                key_span("w", s),
                Span::raw(" Worktrees  "),
                key_span("q", s),
                Span::raw(" Quit"),
            ]
        };

        let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(s.bar_default));
        frame.render_widget(bar, content_area);
        Ok(())
    }
}

fn dim_sep(theme: &StatusBarTheme) -> Span<'static> {
    Span::styled("│ ", Style::default().fg(theme.dim_separator))
}

fn key_span<'a>(key: &'a str, theme: &StatusBarTheme) -> Span<'a> {
    Span::styled(
        format!(" {} ", key),
        Style::default()
            .fg(theme.key_hint_fg)
            .bg(theme.key_hint_bg)
            .add_modifier(Modifier::BOLD),
    )
}
