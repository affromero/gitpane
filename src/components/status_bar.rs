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
            // Onboarding: explain the layout
            match self.right_pane {
                RightPane::FileList => vec![
                    Span::styled(
                        " Left: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("repos ("),
                    key_span("j/k"),
                    Span::raw(" navigate)  "),
                    Span::styled(
                        "Right: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("changed files for selected repo  "),
                    key_span("g"),
                    Span::raw(" git graph  "),
                    key_span("r"),
                    Span::raw(" refresh  "),
                    key_span("q"),
                    Span::raw(" quit  "),
                    Span::styled("↑push ↓pull", Style::default().fg(Color::DarkGray)),
                ],
                RightPane::GitGraph => vec![
                    Span::styled(
                        "Git Graph: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("commit history with branch lanes  "),
                    key_span("j/k"),
                    Span::raw(" scroll  "),
                    key_span("Esc"),
                    Span::raw(" back to files  "),
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

fn key_span(key: &str) -> Span<'_> {
    Span::styled(
        format!(" {} ", key),
        Style::default()
            .fg(Color::Black)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}
