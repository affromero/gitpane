use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use ratatui::style::Color;

fn press(position: Position) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: position.x,
        row: position.y,
        modifiers: KeyModifiers::NONE,
    }
}

fn panel() -> (Buffer, Rect) {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 6));
    let area = Rect::new(3, 1, 8, 3);
    for (row, text) in [(1, "abcdefgh"), (2, "ij kl"), (3, "mnop")] {
        buffer.set_string(area.x, row, text, Style::default());
        buffer.set_string(0, row, "│││", Style::default());
        buffer.set_string(area.right(), row, "│ADJACENT", Style::default());
    }
    (buffer, area)
}

#[test]
fn forward_and_reverse_selection_copy_the_same_visible_cells() {
    for (anchor, head, expected) in [
        (Position::new(4, 1), Position::new(7, 1), "bcde"),
        (
            Position::new(5, 1),
            Position::new(5, 3),
            "cdefgh\nij kl\nmno",
        ),
    ] {
        for (start, end) in [(anchor, head), (head, anchor)] {
            let (mut buffer, area) = panel();
            let mut selection = TextSelection::default();
            selection.begin(area, press(start));
            selection.draw(&mut buffer, Some(area));
            selection.extend(end);
            assert_eq!(selection.copy_text().as_deref(), Some(expected));
            assert!(selection.release(end).is_none());
            assert_eq!(selection.take_copy().as_deref(), Some(expected));
            assert!(selection.take_copy().is_none(), "release copies only once");
        }
    }
}

#[test]
fn selection_clamps_to_its_pane_and_never_highlights_adjacent_cells() {
    let (buffer, area) = panel();
    for (anchor, head) in [
        (Position::new(3, 1), Position::new(u16::MAX, u16::MAX)),
        (Position::new(10, 3), Position::new(0, 0)),
    ] {
        let mut painted = buffer.clone();
        let mut selection = TextSelection::default();
        selection.begin(area, press(anchor));
        selection.draw(&mut painted, Some(area));
        selection.extend(head);
        selection.draw(&mut painted, Some(area));
        assert_eq!(
            selection.copy_text().as_deref(),
            Some("abcdefgh\nij kl\nmnop")
        );
        for row in 0..painted.area.height {
            for column in 0..painted.area.width {
                assert_eq!(
                    painted[(column, row)].modifier.contains(Modifier::REVERSED),
                    area.contains(Position::new(column, row)),
                    "highlight at {column},{row}"
                );
            }
        }
    }
}

#[test]
fn unicode_selection_keeps_wide_and_combining_graphemes_whole() {
    let area = Rect::new(2, 1, 12, 1);
    let mut original = Buffer::empty(Rect::new(0, 0, 18, 3));
    original.set_string(2, 1, "A界e\u{301}👩‍💻Z", Style::default());
    // A=2, 界=3..4, é=5, 👩‍💻=6..7, Z=8. Endpoints may
    // land on continuation cells; the graphemes still copy and highlight whole.
    for (anchor, head, expected, columns) in [
        (
            Position::new(4, 1),
            Position::new(7, 1),
            "界e\u{301}👩‍💻",
            3..8,
        ),
        (
            Position::new(7, 1),
            Position::new(4, 1),
            "界e\u{301}👩‍💻",
            3..8,
        ),
        (Position::new(4, 1), Position::new(3, 1), "界", 3..5),
    ] {
        let mut buffer = original.clone();
        let mut selection = TextSelection::default();
        selection.begin(area, press(anchor));
        selection.draw(&mut buffer, Some(area));
        selection.extend(head);
        selection.draw(&mut buffer, Some(area));
        assert_eq!(selection.copy_text().as_deref(), Some(expected));
        for column in area.x..area.right() {
            assert_eq!(
                buffer[(column, 1)].modifier.contains(Modifier::REVERSED),
                columns.contains(&column)
            );
        }
    }
}

#[test]
fn a_click_or_stationary_drag_never_copies() {
    let (mut buffer, area) = panel();
    let origin = Position::new(5, 2);
    let mut selection = TextSelection::default();
    selection.begin(area, press(origin));
    selection.draw(&mut buffer, Some(area));
    selection.extend(origin);
    assert_eq!(selection.release(origin), Some(press(origin)));
    assert!(selection.take_copy().is_none());
    assert!(!selection.has_selection());
}

#[test]
fn content_or_geometry_changes_clear_selection_but_style_changes_do_not() {
    let (original, area) = panel();
    let mut selection = TextSelection::default();
    selection.begin(area, press(Position::new(4, 1)));
    selection.draw(&mut original.clone(), Some(area));
    selection.extend(Position::new(7, 1));
    let mut restyled = original.clone();
    restyled[(4, 1)].set_fg(Color::Red);
    selection.draw(&mut restyled, Some(area));
    assert_eq!(selection.copy_text().as_deref(), Some("bcde"));
    let mut changed = original.clone();
    changed[(9, 3)].set_symbol("!");
    selection.draw(&mut changed, Some(area));
    assert!(selection.origin().is_none());
    assert!(selection.copy_text().is_none());

    selection.begin(area, press(Position::new(4, 1)));
    selection.draw(&mut original.clone(), Some(area));
    selection.extend(Position::new(7, 1));
    selection.draw(&mut original.clone(), Some(Rect::new(3, 1, 7, 3)));
    assert!(selection.copy_text().is_none());
}

#[test]
fn release_before_snapshot_copies_after_the_next_draw_once() {
    let (mut buffer, area) = panel();
    let mut selection = TextSelection::default();
    selection.begin(area, press(Position::new(4, 1)));
    assert!(selection.release(Position::new(7, 1)).is_none());
    assert!(selection.take_copy().is_none());
    selection.draw(&mut buffer, Some(area));
    assert_eq!(selection.take_copy().as_deref(), Some("bcde"));
    assert!(selection.take_copy().is_none());
}
