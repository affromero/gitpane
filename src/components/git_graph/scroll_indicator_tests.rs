//! Scroll indicators of the commit-detail panes: a long diff, message or file
//! list used to give no signal that the pane had reached its end, and no way to
//! tell a pane that fits from one that is hiding rows.
//!
//! These tests drive the real widget through `draw` and assert on the frame the
//! user sees — the `seen/total` counter in the pane title and the thumb in the
//! pane's last inner column — so the widths the panes reserve and the offsets
//! they clamp to are covered together.

use super::tests::{MOCK_OID, mock_row};
use super::*;
use crate::components::Component;
use crate::components::scroll_pane::THUMB;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Terminal, backend::TestBackend};

const WIDTH: u16 = 80;
const HEIGHT: u16 = 40;

/// A graph with the commit detail open on `files` changed files, a message of
/// `message_lines` lines and `diff_lines` of diff loaded. Panels are stacked
/// (`horizontal_layout`), so every detail pane is as wide as the terminal and the
/// wrap width is predictable.
fn detail_with(message_lines: usize, files: usize, diff_lines: usize) -> GitGraph {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.horizontal_layout = true;
    let message = (0..message_lines)
        .map(|i| format!("message line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let changed = (0..files)
        .map(|i| ("M".to_string(), format!("file{i}.rs")))
        .collect();
    let _ = graph.set_commit_files(MOCK_OID.to_string(), message, changed);
    graph.set_commit_diff(
        (0..diff_lines)
            .map(|i| format!("+line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    graph
}

fn draw(graph: &mut GitGraph) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal
        .draw(|frame| graph.draw(frame, frame.area()).unwrap())
        .unwrap();
    terminal
}

/// The rendered frame, row by row.
fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    rows(terminal).join("\n")
}

/// The scroll-indicator column inside a bordered pane, top row first.
fn indicator_column(terminal: &Terminal<TestBackend>, pane: Rect) -> String {
    let rows = rows(terminal);
    let x = usize::from(pane.x + pane.width - 2);
    (pane.y + 1..pane.y + pane.height - 1)
        .map(|y| rows[usize::from(y)].chars().nth(x).unwrap_or(' '))
        .collect()
}

/// Rows of the pane that are not borders.
fn visible_rows(pane: Rect) -> u16 {
    pane.height - 2
}

#[test]
fn commit_diff_counter_tracks_the_scroll_and_stops_at_the_end() {
    let mut graph = detail_with(2, 1, 50);
    let terminal = draw(&mut graph);
    let visible = visible_rows(graph.diff_area);
    assert!(
        visible > 0 && visible < 50,
        "pane shows {visible} of 50 rows"
    );

    // At the top, the counter counts what the viewport already covers.
    assert!(
        text(&terminal).contains(&format!("{visible}/50")),
        "expected a counter in the diff title: {:?}",
        rows(&terminal)[usize::from(graph.diff_area.y)]
    );
    // And the thumb starts at the top of the pane's indicator column.
    assert!(
        indicator_column(&terminal, graph.diff_area).starts_with(THUMB),
        "thumb should start the track: {:?}",
        indicator_column(&terminal, graph.diff_area)
    );

    // Enter hands the keyboard to the diff. Scrolling far past the end parks on
    // the last screenful instead of running into blank space.
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Enter))
        .unwrap();
    for _ in 0..60 {
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Char('j')))
            .unwrap();
    }
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().diff_scroll,
        50 - visible,
        "the offset must stop one screenful before the end"
    );

    let terminal = draw(&mut graph);
    assert!(
        text(&terminal).contains("50/50"),
        "reaching the end must read as total/total"
    );
    assert!(
        indicator_column(&terminal, graph.diff_area).ends_with(THUMB),
        "thumb should end the track: {:?}",
        indicator_column(&terminal, graph.diff_area)
    );
}

#[test]
fn commit_message_counter_appears_when_the_message_outgrows_its_pane() {
    let mut graph = detail_with(30, 1, 10);
    let terminal = draw(&mut graph);
    let visible = visible_rows(graph.msg_area);
    assert!(visible < 30, "message pane caps at {visible} rows");
    assert!(
        text(&terminal).contains(&format!("{visible}/30")),
        "expected a counter in the message title: {:?}",
        rows(&terminal)[usize::from(graph.msg_area.y)]
    );
    assert!(
        indicator_column(&terminal, graph.msg_area).contains(THUMB),
        "message pane should show a thumb"
    );
}

#[test]
fn commit_file_list_counter_tracks_the_highlight_to_the_end() {
    let mut graph = detail_with(2, 30, 10);
    let terminal = draw(&mut graph);
    let visible = visible_rows(graph.files_area);
    assert!(
        visible > 0 && visible < 30,
        "pane shows {visible} of 30 files"
    );
    assert!(
        text(&terminal).contains(&format!("{visible}/30")),
        "expected a counter in the files title: {:?}",
        rows(&terminal)[usize::from(graph.files_area.y)]
    );
    assert!(
        indicator_column(&terminal, graph.files_area).starts_with(THUMB),
        "files pane should show a thumb"
    );

    // Walking to the last file scrolls the list to its end.
    for _ in 0..40 {
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Char('j')))
            .unwrap();
    }
    let terminal = draw(&mut graph);
    assert!(
        text(&terminal).contains("30/30"),
        "the last file must read as total/total: {:?}",
        rows(&terminal)[usize::from(graph.files_area.y)]
    );
    assert!(
        indicator_column(&terminal, graph.files_area).ends_with(THUMB),
        "thumb should end the track: {:?}",
        indicator_column(&terminal, graph.files_area)
    );
}

#[test]
fn panes_that_fit_show_no_indicator() {
    let mut graph = detail_with(2, 2, 3);
    let terminal = draw(&mut graph);
    let rows = rows(&terminal);
    // Nothing overflows: no thumb in any pane's indicator column, and no counter
    // in any pane title. (Asserted per pane: the lane graph legitimately draws
    // box-drawing glyphs of its own elsewhere in the frame.)
    for pane in [graph.msg_area, graph.files_area, graph.diff_area] {
        assert!(
            !indicator_column(&terminal, pane).contains(THUMB),
            "pane at {:?} shows a thumb: {:?}",
            pane,
            indicator_column(&terminal, pane)
        );
        let title = &rows[usize::from(pane.y)];
        assert!(!has_counter(title), "pane title shows a counter: {title:?}");
    }
}

/// Whether a title row carries a `seen/total` counter.
fn has_counter(row: &str) -> bool {
    row.split_whitespace().any(|token| {
        let Some((seen, total)) = token.split_once('/') else {
            return false;
        };
        !seen.is_empty()
            && seen.bytes().all(|b| b.is_ascii_digit())
            && !total.is_empty()
            && total.bytes().all(|b| b.is_ascii_digit())
    })
}
