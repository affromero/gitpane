use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::action::Action;

pub(crate) struct ConfirmDialog {
    pub visible: bool,
    message: String,
    pending_action: Option<Action>,
}

impl ConfirmDialog {
    pub fn new() -> Self {
        Self {
            visible: false,
            message: String::new(),
            pending_action: None,
        }
    }

    pub fn show(&mut self, message: String, action: Action) {
        self.visible = true;
        self.message = message;
        self.pending_action = Some(action);
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.message.clear();
        self.pending_action = None;
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let action = self.pending_action.take();
                self.hide();
                Ok(action)
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.hide();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let width = 40u16.min(area.width.saturating_sub(4));
        let height = 5u16;

        let [vert] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(vert);

        frame.render_widget(Clear, rect);

        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                &self.message,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled(
                    " y",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::styled("Enter ", Style::default().fg(Color::Green)),
                Span::raw("confirm   "),
                Span::styled(
                    "n",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::styled("Esc ", Style::default().fg(Color::Red)),
                Span::raw("cancel"),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Confirm ");

        let paragraph = Paragraph::new(lines).centered().block(block);
        frame.render_widget(paragraph, rect);
    }
}
