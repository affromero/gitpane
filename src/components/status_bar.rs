use color_eyre::Result;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Instant;

use crate::app::RightPane;
use crate::components::Component;

pub(crate) struct StatusBar {
    started_at: Instant,
    pub right_pane: RightPane,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            right_pane: RightPane::FileList,
        }
    }
}

impl Component for StatusBar {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let elapsed = self.started_at.elapsed().as_secs();

        let spans = if elapsed < 60 {
            // Onboarding: symbol legend + keybindings
            match self.right_pane {
                RightPane::FileList => vec![
                    Span::styled(" * ", Style::default().fg(Color::Yellow)),
                    Span::styled("dirty  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("↑", Style::default().fg(Color::Green)),
                    Span::styled("push ", Style::default().fg(Color::DarkGray)),
                    Span::styled("↓", Style::default().fg(Color::Red)),
                    Span::styled("pull  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("[n]", Style::default().fg(Color::Yellow)),
                    Span::styled(" changed files  ", Style::default().fg(Color::DarkGray)),
                    dim_sep(),
                    key_span("Tab"),
                    Span::raw(" switch pane  "),
                    key_span("Enter"),
                    Span::raw(" diff  "),
                    key_span("g"),
                    Span::raw(" graph  "),
                    key_span("q"),
                    Span::raw(" quit"),
                ],
                RightPane::GitGraph => vec![
                    Span::styled("● ", Style::default().fg(Color::Cyan)),
                    Span::styled("commit  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("│ ", Style::default().fg(Color::Green)),
                    Span::styled("branch lane  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("╭╮", Style::default().fg(Color::Yellow)),
                    Span::styled(" merge/fork  ", Style::default().fg(Color::DarkGray)),
                    dim_sep(),
                    key_span("j/k"),
                    Span::raw(" scroll  "),
                    key_span("Esc"),
                    Span::raw(" back  "),
                    key_span("q"),
                    Span::raw(" quit"),
                ],
            }
        } else {
            // Compact mode after 60s
            match self.right_pane {
                RightPane::FileList => vec![
                    key_span("j/k"),
                    Span::raw(" Navigate  "),
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
                ],
                RightPane::GitGraph => vec![
                    key_span("j/k"),
                    Span::raw(" Scroll  "),
                    key_span("Esc"),
                    Span::raw(" Back  "),
                    key_span("q"),
                    Span::raw(" Quit"),
                ],
            }
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
