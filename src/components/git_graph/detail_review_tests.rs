//! Regression tests from the commit-detail-panes PR review (issues #68).
//!
//! Two bugs were reported during review and are covered here through the
//! actual widget draw and mouse paths (not just the pure layout helpers):
//!   1. P1 — drawing commit detail panicked at an ordinary 80x22 terminal
//!      because the stored 0.40/0.65 split left an inverted clamp range.
//!   2. P2 — `max_scroll_lines` counted Unicode scalar values while the
//!      paragraph wraps by display width, so a CJK message's end was
//!      unreachable (wheel scroll stuck before the last line).

use super::tests::{MOCK_OID, mock_row};
use super::*;
use crate::components::Component;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Terminal, backend::TestBackend};

/// A graph with one commit, horizontal detail layout, and a message made of 100
/// CJK glyphs followed by `END`, whose diff is loaded — the reviewer's scenario.
fn graph_with_unicode_detail() -> GitGraph {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.horizontal_layout = true;
    let msg = "中".repeat(100) + "END";
    let _ = graph.set_commit_files(
        MOCK_OID.to_string(),
        msg,
        vec![("a.rs".to_string(), "M".to_string())],
    );
    graph.set_commit_diff("line\n".repeat(50));
    graph
}

#[test]
fn review_commit_detail_renders_at_80_by_22() {
    let mut graph = graph_with_unicode_detail();
    let mut terminal = Terminal::new(TestBackend::new(80, 22)).unwrap();
    // Must not panic (the default split no longer inverts against a small area).
    terminal.draw(|f| graph.draw(f, f.area()).unwrap()).unwrap();
}

#[test]
fn review_wheel_reaches_end_of_wrapped_unicode_message() {
    let mut graph = graph_with_unicode_detail();
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
    // Draw once so the message pane geometry is computed.
    terminal.draw(|f| graph.draw(f, f.area()).unwrap()).unwrap();

    // Wheel down over the message pane ten times; the offset must stop at the
    // content end (wrapped by display width), not get stuck well before it.
    let col = graph.msg_area.x + 1;
    let row = graph.msg_area.y + 1;
    for _ in 0..10 {
        graph
            .handle_mouse_event(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();
    }

    let scroll = graph.commit_detail.as_ref().unwrap().msg_scroll;
    let inner = graph.msg_area.width - 2;
    let content_rows = Paragraph::new(graph.commit_detail.as_ref().unwrap().message.as_str())
        .wrap(Wrap { trim: false })
        .line_count(inner) as u16;
    let expected = content_rows.saturating_sub(graph.msg_area.height - 2);
    assert_eq!(
        scroll, expected,
        "message scroll must reach the content end (wrapped by display width)"
    );
}

#[test]
fn review_dragging_a_border_at_small_area_does_not_panic() {
    let mut graph = graph_with_unicode_detail();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 22)).unwrap();
    terminal.draw(|f| graph.draw(f, f.area()).unwrap()).unwrap();

    // Grab the message|files border (border 1). On the buggy code this is the
    // drag that inverted the clamp range: `split[1] = rel.clamp(split[0] + min_f,
    // split[2] - min_f)` against the raw 0.40/0.65 split gave a min (0.536) >
    // max (0.513) range and panicked. Drag it far toward the top, then draw again
    // so the no-panic contract is exercised through the real draw path.
    let border_row = graph.msg_area.y + graph.msg_area.height;
    let _ = graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: graph.msg_area.x + 1,
            row: border_row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
    let _ = graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: graph.msg_area.x + 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

    // Dragging border 1 pins the message height: auto-size must stop.
    assert!(graph.msg_dragged, "dragging border 1 must set msg_dragged");

    // The stored split must stay ordered (borders never cross) and the next
    // frame must draw without panicking.
    let [a, b, c] = graph.detail_split;
    assert!(a < b && b < c, "split must stay ordered: {a} < {b} < {c}");
    assert!(a >= 0.0 && c <= 1.0, "split must stay in range: {a}..{c}");
    terminal.draw(|f| graph.draw(f, f.area()).unwrap()).unwrap();
}
