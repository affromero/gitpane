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
use crate::components::scroll_pane::{self, ScrollLayout, THUMB};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

/// Send one mouse event to the graph, the way `App` routes them.
fn mouse(graph: &mut GitGraph, kind: MouseEventKind, column: u16, row: u16) -> Option<Action> {
    graph
        .handle_mouse_event(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap()
}

/// The indicator column's x for a pane (its last inner column).
fn bar_x(pane: Rect) -> u16 {
    pane.x + pane.width - 2
}

/// The largest diff offset the pane as drawn allows.
fn diff_max(graph: &GitGraph) -> u16 {
    let detail = graph.commit_detail.as_ref().unwrap();
    let layout = ScrollLayout::for_text(
        detail.diff_content.as_deref().unwrap(),
        scroll_pane::bordered_inner(graph.diff_area),
        0,
    );
    layout.gauge.max_offset()
}

#[test]
fn dragging_the_diff_indicator_scrubs_the_whole_pane() {
    let mut graph = detail_with(2, 1, 50);
    draw(&mut graph);
    let pane = graph.diff_area;
    let max = diff_max(&graph);
    assert!(max > 10, "the fixture must overflow: {max}");

    // Clicking the top of the track shows the top of the diff.
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 0);
    assert!(
        graph.commit_detail.as_ref().unwrap().diff_focused,
        "grabbing the diff's indicator hands it the keyboard"
    );

    // Clicking the bottom jumps to the end of the content.
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + pane.height - 2,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, max);

    // A drag scrubs: hold the grab and move the pointer up, then past the top.
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 0);
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        bar_x(pane),
        pane.y + pane.height - 2,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, max);
    // Sideways motion keeps scrubbing: only the row matters.
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        pane.x + 1,
        pane.y + 1,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 0);

    // Releasing ends the grab: later motion is not a scrub.
    mouse(
        &mut graph,
        MouseEventKind::Up(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert!(graph.scrubbing.is_none());
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        bar_x(pane),
        pane.y + pane.height - 2,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 0);
}

#[test]
fn dragging_the_files_indicator_walks_the_highlight_to_the_end() {
    let mut graph = detail_with(2, 30, 10);
    draw(&mut graph);
    let pane = graph.files_area;
    let last_row = pane.y + pane.height - 2;
    let visible = usize::from(pane.height - 2);

    // Grabbing the bottom of the track scrolls the list to its last screenful;
    // the highlight follows the top of that screenful, and schedules its diff.
    let action = mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        last_row,
    );
    assert!(
        matches!(action, Some(Action::ScheduleCommitDiff { .. })),
        "the scrub must ask for the newly highlighted file's diff: {action:?}"
    );
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().file_state.selected(),
        Some(30 - visible),
        "the highlight lands on the top row of the last screenful"
    );
    assert!(
        graph.dragging_detail_border.is_none(),
        "the indicator wins over the border grab zone it overlaps, so the drag scrubs"
    );
    let terminal = draw(&mut graph);
    assert!(
        text(&terminal).contains("30/30"),
        "the list reached its end: {:?}",
        rows(&terminal)[usize::from(pane.y)]
    );

    // Dragging back to the top returns to the first file.
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().file_state.selected(),
        Some(0)
    );
}

#[test]
fn clicking_the_message_indicator_scrolls_the_message() {
    let mut graph = detail_with(30, 1, 10);
    draw(&mut graph);
    let pane = graph.msg_area;
    let track_last = pane.y + pane.height - 2;

    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().msg_scroll, 0);

    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        track_last,
    );
    let message = graph.commit_detail.as_ref().unwrap().message.clone();
    let expected = ScrollLayout::for_text(&message, scroll_pane::bordered_inner(pane), 0)
        .gauge
        .max_offset();
    assert!(
        expected > 0,
        "the fixture message must overflow its pane to be scrubbable"
    );
    assert_eq!(graph.commit_detail.as_ref().unwrap().msg_scroll, expected);
}

#[test]
fn clicking_a_file_row_beside_the_indicator_still_selects_it() {
    let mut graph = detail_with(2, 30, 10);
    draw(&mut graph);
    let pane = graph.files_area;
    // The content column nearest the indicator, two rows down the list.
    let action = mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        pane.x + pane.width - 3,
        pane.y + 2,
    );
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().file_state.selected(),
        Some(1),
        "a row click must still select the row: {action:?}"
    );
    assert!(
        graph.scrubbing.is_none(),
        "a row click must not start an indicator scrub"
    );
}

#[test]
fn a_click_scrubs_the_pane_whose_track_was_grabbed() {
    // Stacked panes share the indicator's column, so the row is what says which
    // pane was grabbed: with all three overflowing, each thumb must move only its
    // own offset.
    let mut graph = detail_with(30, 30, 50);
    draw(&mut graph);
    let (message, files, diff) = (graph.msg_area, graph.files_area, graph.diff_area);
    assert_eq!(
        (bar_x(message), bar_x(files), bar_x(diff)),
        (bar_x(diff), bar_x(diff), bar_x(diff)),
        "stacked panes share the indicator column"
    );
    let max = diff_max(&graph);

    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(diff),
        diff.y + diff.height - 2,
    );
    let detail = graph.commit_detail.as_ref().unwrap();
    assert_eq!(detail.diff_scroll, max, "the diff jumped to its end");
    assert_eq!(detail.msg_scroll, 0, "the message must be untouched");
    assert_eq!(
        detail.file_state.selected(),
        Some(0),
        "and so must the file list"
    );

    // Grabbing the message's own track moves only the message.
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(message),
        message.y + message.height - 2,
    );
    let detail = graph.commit_detail.as_ref().unwrap();
    assert!(detail.msg_scroll > 0, "the message scrolled");
    assert_eq!(detail.diff_scroll, max, "the diff stays put");
}

#[test]
fn a_commit_row_click_at_the_indicator_column_still_selects_the_commit() {
    // The graph list shares the indicator's column with the detail panes below it
    // (they are stacked), and it has no indicator of its own: its rows must stay
    // clickable.
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice"), {
        // A different oid, so clicking this row is a fresh open rather than the
        // no-op a click on the already-open commit is.
        let mut second = mock_row("def5678", "second", "Bob");
        second.oid = git2::Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        second
    }]);
    graph.horizontal_layout = true;
    let message = (0..30)
        .map(|i| format!("message line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = graph.set_commit_files(
        MOCK_OID.to_string(),
        message,
        vec![("M".to_string(), "a.rs".to_string())],
    );
    graph.set_commit_diff("diff --git a/a.rs\n".to_string());
    draw(&mut graph);

    // Second row of the graph list, on the indicator's column.
    let row = graph.graph_list_area.y + 2;
    let column = bar_x(graph.graph_list_area);
    let action = mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
    );
    assert_eq!(
        graph.state.selected(),
        Some(1),
        "the commit row must be selected, not a pane scrubbed: {action:?}"
    );
    assert!(
        !graph.has_detail(),
        "the row click opened that commit's files, so it reached the list"
    );
    assert!(graph.scrubbing.is_none(), "no indicator grab started");
}

#[test]
fn a_cancelled_grab_stops_scrubbing() {
    // Mouse events route by pointer position, so a press released over another
    // panel is never delivered here; the app cancels the stale grab instead.
    let mut graph = detail_with(2, 1, 50);
    draw(&mut graph);
    let pane = graph.diff_area;
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert!(graph.scrubbing.is_some(), "the press armed the grab");

    graph.cancel_drag();
    assert!(graph.scrubbing.is_none());
    mouse(
        &mut graph,
        MouseEventKind::Drag(MouseButton::Left),
        bar_x(pane),
        pane.y + pane.height - 2,
    );
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().diff_scroll,
        0,
        "a cancelled grab must not scrub"
    );
}

#[test]
fn a_scrub_with_a_filter_moves_the_highlight_between_matches() {
    // The list draws every row (matches are only bolded), so a scrub aimed between
    // matches has to snap to a close one instead of dying on a row the filter
    // excludes. Asserted as properties, since the filter's own search overlay
    // changes the pane's height while it is active.
    let mut graph = detail_with(2, 30, 10);
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Char('/')))
        .unwrap();
    for c in "file2".chars() {
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Char(c)))
            .unwrap();
    }
    // Enter ends the search on the first match: file2.rs, index 2.
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Enter))
        .unwrap();
    draw(&mut graph);
    let pane = graph.files_area;
    let detail = graph.commit_detail.as_ref().unwrap();
    let matches = detail.file_filter.matches().to_vec();
    assert_eq!(
        detail.file_state.selected(),
        Some(2),
        "the search selected the first match"
    );
    // The bottom of the track has to aim between matches, or the press below would
    // not need the snap at all (and the test would pass without the fix). The
    // filter's search overlay changes the pane's height, so this is asserted
    // rather than assumed.
    let bottom_row =
        ScrollLayout::for_list(detail.files.len(), scroll_pane::bordered_inner(pane), 0)
            .gauge
            .max_offset();
    assert!(
        !detail.file_matches(usize::from(bottom_row)),
        "fixture: row {bottom_row} must be excluded by the filter"
    );

    // Bottom of the track: the highlight has to move down to a matching row.
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + pane.height - 2,
    );
    let selected = graph
        .commit_detail
        .as_ref()
        .unwrap()
        .file_state
        .selected()
        .unwrap();
    assert!(
        matches.contains(&selected) && selected > 2,
        "the scrub must land on a match below the search's pick: {selected} of {matches:?}"
    );

    // Top of the track: back to the first match.
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().file_state.selected(),
        Some(2),
        "the top of the track is the search's own pick again"
    );
}

#[test]
fn a_scrub_on_the_highlighted_row_still_asks_for_its_diff() {
    // A failed or still-running diff load leaves the highlight on row 0 with no
    // content: grabbing the indicator there must ask again, the way a row click
    // does, instead of being skipped as a no-op.
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.horizontal_layout = true;
    let files = (0..30)
        .map(|i| ("M".to_string(), format!("file{i}.rs")))
        .collect();
    let _ = graph.set_commit_files(MOCK_OID.to_string(), "message".to_string(), files);
    assert!(
        graph.commit_detail.as_ref().unwrap().diff_content.is_none(),
        "the fixture is the state a pending load leaves behind"
    );
    draw(&mut graph);

    let pane = graph.files_area;
    let action = mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert!(
        matches!(action, Some(Action::ScheduleCommitDiff { .. })),
        "the grab must ask for the highlighted file's diff: {action:?}"
    );
}

#[test]
fn a_scrub_on_the_highlighted_row_returns_the_keyboard_to_the_list() {
    // Grabbing the list's indicator is interacting with the list, so the keyboard
    // comes back from the diff — the way a row click hands it back — even when the
    // grab lands on the row that is already highlighted.
    let mut graph = detail_with(2, 30, 10);
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Enter))
        .unwrap();
    assert!(
        graph.commit_detail.as_ref().unwrap().diff_focused,
        "Enter hands the keyboard to the diff"
    );
    draw(&mut graph);

    let pane = graph.files_area;
    mouse(
        &mut graph,
        MouseEventKind::Down(MouseButton::Left),
        bar_x(pane),
        pane.y + 1,
    );
    assert!(
        !graph.commit_detail.as_ref().unwrap().diff_focused,
        "the grab must return the keyboard to the file list"
    );
}
