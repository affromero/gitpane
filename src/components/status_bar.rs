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
    pub inbox_hint: Option<usize>,
    /// When set (e.g. the graph filter picker is open), the repo-status
    /// legend on the left is replaced by this hint so key help stays visible
    /// in the most overlooked corner of the screen.
    pub overlay_hint: Option<String>,
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
            inbox_hint: None,
            overlay_hint: None,
            theme,
        }
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }
}

/// Whether a toast is in the "on" half of its attention pulse: the style
/// flips every 150ms for the toast's whole lifetime, so it keeps drawing the
/// eye until it expires.
fn pulse_on(elapsed_ms: u128) -> bool {
    (elapsed_ms / 150).is_multiple_of(2)
}

/// Toast line with the pulse applied: while the pulse is on, the message
/// swaps fg/bg (REVERSED), which flashes it in the accent color.
fn toast_line<'a>(
    label: &'a str,
    label_style: Style,
    msg: String,
    msg_style: Style,
    when: Instant,
) -> Line<'a> {
    let msg_style = if pulse_on(when.elapsed().as_millis()) {
        msg_style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        msg_style
    };
    Line::from(vec![
        Span::styled(label, label_style),
        Span::styled(msg, msg_style),
    ])
}

impl Component for StatusBar {
    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let s = &self.theme.status_bar;
        let r = &self.theme.repo_list;
        let version_text = format!("v{} ", gitpane::VERSION);
        let version_len = version_text.len() as u16;
        let inbox = self.inbox_hint.map(|count| {
            let label = match count {
                0 => "F6 Inbox  ".to_owned(),
                50.. => "🔔 50+  F6 Inbox  ".to_owned(),
                count => format!("🔔 {count}  F6 Inbox  "),
            };
            Line::styled(
                label,
                Style::default().fg(if count > 0 { r.dirty_marker } else { s.version }),
            )
        });
        let inbox_len = inbox.as_ref().map_or(0, |line| line.width() as u16);
        let chunks = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(inbox_len),
            Constraint::Length(version_len),
        ])
        .split(area);
        let content_area = chunks[0];
        let version_area = chunks[2];
        if let Some(inbox) = inbox {
            frame.render_widget(Paragraph::new(inbox).right_aligned(), chunks[1]);
        }

        let version = Paragraph::new(version_text)
            .style(Style::default().fg(s.version))
            .right_aligned();
        frame.render_widget(version, version_area);

        if let Some((ref msg, when)) = self.error {
            if when.elapsed().as_secs() < 5 {
                let error_bar = Paragraph::new(toast_line(
                    " ERROR ",
                    Style::default()
                        .fg(s.error_label_fg)
                        .bg(s.error_label_bg)
                        .add_modifier(Modifier::BOLD),
                    format!(" {}", msg),
                    Style::default().fg(s.error_text),
                    when,
                ));
                frame.render_widget(error_bar, content_area);
                return Ok(());
            } else {
                self.error = None;
            }
        }

        if let Some((ref msg, when)) = self.success {
            if when.elapsed().as_secs() < 3 {
                let success_bar = Paragraph::new(toast_line(
                    " OK ",
                    Style::default()
                        .fg(s.success_label_fg)
                        .bg(s.success_label_bg)
                        .add_modifier(Modifier::BOLD),
                    format!(" {}", msg),
                    Style::default().fg(s.success_text),
                    when,
                ));
                frame.render_widget(success_bar, content_area);
                return Ok(());
            } else {
                self.success = None;
            }
        }

        let elapsed = self.started_at.elapsed().as_secs();

        let spans: Vec<Span<'_>> = if let Some(hint) = self.overlay_hint.as_deref() {
            // Modal (e.g. graph filter picker) is open: replace the repo
            // legend with the picker hint, independent of the 60s legend
            // window so it never vanishes mid-filtering.
            let mut v = vec![
                Span::styled(hint.to_string(), Style::default().fg(s.legend_text)),
                dim_sep(s),
            ];
            v.extend(key_hint_spans(s, self.sort_order.label()));
            v
        } else if elapsed < 60 {
            let mut v: Vec<Span<'_>> = vec![
                key_span("p", s),
                Span::raw(" Pull  "),
                key_span("P", s),
                Span::raw(" Push  "),
                dim_sep(s),
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
            ];
            v.push(dim_sep(s));
            v.extend(key_hint_spans(s, self.sort_order.label()));
            v
        } else {
            let focus_label = match self.focus {
                FocusPanel::Repos => "Repos",
                FocusPanel::Changes => "Changes",
                FocusPanel::Graph => "Graph",
                FocusPanel::GitHub => "GitHub",
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
                key_span("p", s),
                Span::raw(" Pull  "),
                key_span("P", s),
                Span::raw(" Push  "),
                key_span("Tab", s),
                Span::raw(" Switch  "),
                key_span("Enter", s),
                Span::raw(" Diff  "),
                key_span("g", s),
                Span::raw(" Graph  "),
                key_span("=", s),
                Span::raw(" GitHub  "),
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
                key_span("S", s),
                Span::raw(" Stash  "),
                key_span("t", s),
                Span::raw(" Theme  "),
                key_span("q", s),
                Span::raw(" Quit"),
            ]
        };

        let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(s.bar_default));
        frame.render_widget(bar, content_area);
        Ok(())
    }
}

/// The always-on key help tail shared by the status bar variants.
fn key_hint_spans<'a>(s: &'a StatusBarTheme, sort_label: &'a str) -> Vec<Span<'a>> {
    vec![
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
        Span::raw(format!(" sort ({})  ", sort_label)),
        key_span("w", s),
        Span::raw(" worktrees  "),
        key_span("S", s),
        Span::raw(" stash list  "),
        key_span("t", s),
        Span::raw(" theme  "),
        key_span("q", s),
        Span::raw(" quit"),
    ]
}

fn dim_sep(theme: &StatusBarTheme) -> Span<'static> {
    Span::styled("│ ", Style::default().fg(theme.dim_separator))
}

fn key_span<'a>(key: &'a str, theme: &StatusBarTheme) -> Span<'a> {
    Span::styled(
        format!(" {} ", key),
        Style::default().fg(theme.key_hint_fg).bg(theme.key_hint_bg),
    )
}

#[cfg(test)]
mod tests {
    use super::{StatusBar, pulse_on};
    use crate::{components::Component, theme::Theme};
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier},
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    #[test]
    fn key_hints_keep_black_text_distinct_from_bright_black_background() {
        for variant in 0..3 {
            let mut bar = StatusBar::new(Arc::new(Theme::default()));
            if variant == 1 {
                bar.overlay_hint = Some("Filter".to_string());
            } else if variant == 2 {
                bar.started_at = Instant::now() - Duration::from_secs(61);
            }
            let mut terminal = Terminal::new(TestBackend::new(300, 1)).unwrap();
            terminal
                .draw(|frame| bar.draw(frame, frame.area()).unwrap())
                .unwrap();
            let buffer = terminal.backend().buffer();
            let symbols: Vec<_> = (0..300).map(|x| buffer[(x, 0)].symbol()).collect();
            for key in ["Tab", "Enter"] {
                let letters: Vec<_> = key.chars().map(|ch| ch.to_string()).collect();
                let offset = symbols
                    .windows(letters.len())
                    .position(|slice| {
                        slice
                            .iter()
                            .zip(&letters)
                            .all(|(actual, expected)| *actual == expected)
                    })
                    .expect("key hint is visible");
                for x in offset..offset + key.len() {
                    let cell = &buffer[(x as u16, 0)];
                    assert_eq!(cell.fg, Color::Black);
                    assert_eq!(cell.bg, Color::DarkGray);
                    assert!(!cell.modifier.contains(Modifier::BOLD));
                }
            }
        }
    }

    #[test]
    fn inbox_hint_shows_shortcut_and_caps_review_count() {
        let mut bar = StatusBar::new(Arc::new(Theme::default()));
        let mut terminal = Terminal::new(TestBackend::new(140, 1)).unwrap();
        for (count, expected) in [
            (0, "F6 Inbox"),
            (3, "🔔 3  F6 Inbox"),
            (50, "🔔 50+  F6 Inbox"),
        ] {
            bar.inbox_hint = Some(count);
            terminal
                .draw(|frame| bar.draw(frame, frame.area()).unwrap())
                .unwrap();
            let buffer = terminal.backend().buffer();
            let line: String = (0..140)
                .filter(|&x| x == 0 || buffer[(x - 1, 0)].symbol() != "🔔")
                .map(|x| buffer[(x, 0)].symbol())
                .collect();
            assert!(line.contains(expected), "{line}");
            assert_eq!(line.contains('🔔'), count > 0);
        }
    }

    #[test]
    fn pull_and_push_hints_fit_beside_inbox_on_narrow_terminals() {
        let mut bar = StatusBar::new(Arc::new(Theme::default()));
        bar.inbox_hint = Some(3);
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        for elapsed in [0, 61] {
            bar.started_at = Instant::now() - Duration::from_secs(elapsed);
            terminal
                .draw(|frame| bar.draw(frame, frame.area()).unwrap())
                .unwrap();
            let line: String = (0..80)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(line.contains("p  Pull"), "{line}");
            assert!(line.contains("P  Push"), "{line}");
            assert!(line.contains("F6 Inbox"), "{line}");
        }
    }

    #[test]
    fn pulse_alternates_for_the_whole_lifetime() {
        assert!(pulse_on(0));
        assert!(!pulse_on(150));
        assert!(pulse_on(300));
        assert!(!pulse_on(450));
        // No settle point: the pulse keeps going until the toast expires.
        assert!(pulse_on(1200));
        assert!(!pulse_on(4350));
    }
}
