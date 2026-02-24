use color_eyre::Result;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Instant;

use crate::app::FocusPanel;
use crate::components::Component;

pub(crate) struct StatusBar {
    started_at: Instant,
    pub focus: FocusPanel,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            focus: FocusPanel::Repos,
        }
    }
}

impl Component for StatusBar {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
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
