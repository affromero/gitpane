use color_eyre::Result;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Instant;

use crate::app::{FocusPanel, SortOrder};
use crate::components::Component;

pub(crate) struct StatusBar {
    started_at: Instant,
    pub focus: FocusPanel,
    pub sort_order: SortOrder,
    pub error: Option<(String, Instant)>,
    pub success: Option<(String, Instant)>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            focus: FocusPanel::Repos,
            sort_order: SortOrder::Alphabetical,
            error: None,
            success: None,
        }
    }
}

impl Component for StatusBar {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        // Show error for 5 seconds, then clear
        if let Some((ref msg, when)) = self.error {
            if when.elapsed().as_secs() < 5 {
                let error_bar = Paragraph::new(Line::from(vec![
                    Span::styled(
                        " ERROR ",
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::Red)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", msg), Style::default().fg(Color::Red)),
                ]));
                frame.render_widget(error_bar, area);
                return Ok(());
            } else {
                self.error = None;
            }
        }

        // Show success for 3 seconds
        if let Some((ref msg, when)) = self.success {
            if when.elapsed().as_secs() < 3 {
                let success_bar = Paragraph::new(Line::from(vec![
                    Span::styled(
                        " OK ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", msg), Style::default().fg(Color::Green)),
                ]));
                frame.render_widget(success_bar, area);
                return Ok(());
            } else {
                self.success = None;
            }
        }

        let elapsed = self.started_at.elapsed().as_secs();

        let spans = if elapsed < 60 {
            // Onboarding: symbol legend + keybindings
            vec![
                Span::styled(" * ", Style::default().fg(Color::Yellow)),
                Span::styled("dirty ", Style::default().fg(Color::DarkGray)),
                Span::styled("↑", Style::default().fg(Color::Green)),
                Span::styled("push ", Style::default().fg(Color::DarkGray)),
                Span::styled("↓", Style::default().fg(Color::Red)),
                Span::styled("pull ", Style::default().fg(Color::DarkGray)),
                Span::styled("[n]", Style::default().fg(Color::Yellow)),
                Span::styled(" files  ", Style::default().fg(Color::DarkGray)),
                dim_sep(),
                key_span("Tab"),
                Span::raw(" switch  "),
                key_span("Enter"),
                Span::raw(" diff  "),
                key_span("g"),
                Span::raw(" reload graph  "),
                key_span("r"),
                Span::raw(" refresh  "),
                key_span("R"),
                Span::raw(" rescan  "),
                key_span("a"),
                Span::raw(" add  "),
                key_span("d"),
                Span::raw(" remove  "),
                key_span("y"),
                Span::raw(" copy  "),
                key_span("s"),
                Span::raw(format!(" sort ({})  ", self.sort_order.label())),
                key_span("q"),
                Span::raw(" quit"),
            ]
        } else {
            // Compact
            let focus_label = match self.focus {
                FocusPanel::Repos => "Repos",
                FocusPanel::Changes => "Changes",
                FocusPanel::Graph => "Graph",
            };
            vec![
                Span::styled(
                    format!(" {} ", focus_label),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                key_span("Tab"),
                Span::raw(" Switch  "),
                key_span("Enter"),
                Span::raw(" Diff  "),
                key_span("g"),
                Span::raw(" Graph  "),
                key_span("r"),
                Span::raw(" Refresh  "),
                key_span("R"),
                Span::raw(" Rescan  "),
                key_span("a"),
                Span::raw(" Add  "),
                key_span("d"),
                Span::raw(" Remove  "),
                key_span("y"),
                Span::raw(" Copy  "),
                key_span("s"),
                Span::raw(format!(" Sort ({})  ", self.sort_order.label())),
                key_span("q"),
                Span::raw(" Quit"),
            ]
        };

        let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::Gray));
        frame.render_widget(bar, area);
        Ok(())
    }
}

fn dim_sep() -> Span<'static> {
    Span::styled("│ ", Style::default().fg(Color::DarkGray))
}

fn key_span(key: &str) -> Span<'_> {
    Span::styled(
        format!(" {} ", key),
        Style::default()
            .fg(Color::Black)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}
