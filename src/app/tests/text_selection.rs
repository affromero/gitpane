use super::*;
use crate::git::status::{FileEntry, FileStatus, SubmoduleWarn};
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, layout::Position, style::Modifier};

type TestTerminal = ratatui::Terminal<TestBackend>;

fn draw(app: &mut App, terminal: &mut TestTerminal) {
    terminal.draw(|frame| app.draw(frame).unwrap()).unwrap();
}

fn mouse(kind: MouseEventKind, position: Position) -> MouseEvent {
    MouseEvent {
        kind,
        column: position.x,
        row: position.y,
        modifiers: KeyModifiers::NONE,
    }
}

fn find_text(terminal: &TestTerminal, area: Rect, text: &str) -> Position {
    assert!(text.is_ascii());
    let buffer = terminal.backend().buffer();
    let width = text.len() as u16;
    for row in area.y..area.bottom() {
        for column in area.x..=area.right().saturating_sub(width) {
            if (column..column + width)
                .map(|x| buffer[(x, row)].symbol())
                .collect::<String>()
                == text
            {
                return Position::new(column, row);
            }
        }
    }
    panic!("{text:?} is not visible in {area:?}");
}

fn fixture(content: String) -> (App, TestTerminal) {
    let mut app = power_test_app();
    app.file_list.set_diff(content);
    let mut terminal = TestTerminal::new(TestBackend::new(160, 48)).unwrap();
    draw(&mut app, &mut terminal);
    drain_actions(&mut app);
    (app, terminal)
}

fn start_drag(app: &mut App, terminal: &mut TestTerminal, start: Position, end: Position) {
    app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), start))
        .unwrap();
    draw(app, terminal);
    app.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), end))
        .unwrap();
    draw(app, terminal);
}

fn copies(actions: &[Action]) -> Vec<&str> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::CopyText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn text_drag_highlights_and_copies_on_release_with_selection_copy_keys() {
    let (mut app, mut terminal) = fixture("+alpha beta\n+gamma delta".into());
    let line = find_text(&terminal, app.changes_area, "+alpha beta");
    let start = Position::new(line.x + 1, line.y);
    let end = Position::new(line.x + 5, line.y);
    start_drag(&mut app, &mut terminal, start, end);
    assert!(
        copies(&drain_actions(&mut app)).is_empty(),
        "drag does not copy before release"
    );
    for column in start.x..=end.x {
        assert!(
            terminal.backend().buffer()[(column, line.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );
    }
    assert!(
        !terminal.backend().buffer()[(line.x, line.y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), end))
        .unwrap();
    assert_eq!(copies(&drain_actions(&mut app)), ["alpha"]);

    for key in [
        KeyEvent::from(KeyCode::Char('y')),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        app.handle_key_event(key).unwrap();
        let actions = drain_actions(&mut app);
        assert_eq!(copies(&actions), ["alpha"]);
        assert!(!actions.iter().any(|action| matches!(action, Action::Quit)));
    }
    let focus = app.focus;
    app.handle_key_event(KeyCode::Esc.into()).unwrap();
    assert!(app.text_selection.origin().is_none());
    assert_eq!(app.focus, focus, "Esc first clears the selection");
    drain_actions(&mut app);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(
        drain_actions(&mut app)
            .iter()
            .any(|action| matches!(action, Action::Quit))
    );
}

#[test]
fn app_drag_copies_multiline_in_both_directions_and_stops_at_the_pane_edge() {
    let (mut app, mut terminal) = fixture("+alpha beta\n+gamma delta".into());
    let first = find_text(&terminal, app.changes_area, "+alpha beta");
    let second = find_text(&terminal, app.changes_area, "+gamma delta");
    let start = Position::new(first.x + 1, first.y);
    let end = Position::new(second.x + 5, second.y);
    for (anchor, head) in [(start, end), (end, start)] {
        start_drag(&mut app, &mut terminal, anchor, head);
        app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), head))
            .unwrap();
        assert_eq!(copies(&drain_actions(&mut app)), ["alpha beta\n+gamma"]);
    }
    let outside = Position::new(app.graph_area.x + 12, first.y);
    start_drag(&mut app, &mut terminal, start, outside);
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), outside))
        .unwrap();
    assert_eq!(copies(&drain_actions(&mut app)), ["alpha beta"]);
}

fn file(path: &str) -> FileEntry {
    FileEntry {
        path: path.into(),
        status: FileStatus::Modified,
        staged: false,
        unstaged: true,
        is_submodule: false,
        submodule_state: None,
        submodule_warn: SubmoduleWarn::default(),
        submodule_head: None,
    }
}

#[test]
fn single_click_preserves_file_selection_and_opening_without_copying() {
    let mut app = power_test_app();
    app.file_list.set_files(
        vec![file("one"), file("two")],
        "repo",
        RepoId("/repo".into()),
    );
    let mut terminal = TestTerminal::new(TestBackend::new(160, 40)).unwrap();
    draw(&mut app, &mut terminal);
    let text = find_text(&terminal, app.changes_area, "two");
    for expected_before in ["one", "two"] {
        app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), text))
            .unwrap();
        assert_eq!(
            app.file_list.selected_path().as_deref(),
            Some(expected_before)
        );
        draw(&mut app, &mut terminal);
        app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), text))
            .unwrap();
        assert_eq!(app.file_list.selected_path().as_deref(), Some("two"));
        let actions = drain_actions(&mut app);
        assert!(copies(&actions).is_empty());
        assert_eq!(
            actions.iter().any(
                |action| matches!(action, Action::ShowDiff(_, path) if path == Path::new("two"))
            ),
            expected_before == "two"
        );
    }
    app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Right), text))
        .unwrap();
    assert!(drain_actions(&mut app).iter().any(|action| matches!(action,
        Action::ShowFileContextMenu { path, .. } if path == Path::new("two"))));
    assert!(app.text_selection.origin().is_none());
}

#[test]
fn scroll_indicators_wheel_and_panel_borders_keep_their_gestures() {
    let (mut app, mut terminal) = fixture((0..60).map(|row| format!("+line{row}\n")).collect());
    let split = app.border_frac;
    let bar = Position::new(app.changes_area.right() - 2, app.changes_area.bottom() - 2);
    app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), bar))
        .unwrap();
    assert!(app.text_selection.origin().is_none());
    assert!(app.dragging_border.is_none());
    draw(&mut app, &mut terminal);
    find_text(&terminal, app.changes_area, "+line59");
    let top = Position::new(bar.x, app.changes_area.y + 1);
    app.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), top))
        .unwrap();
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), top))
        .unwrap();
    draw(&mut app, &mut terminal);
    let first = find_text(&terminal, app.changes_area, "+line0");
    assert_eq!(app.border_frac, split);
    app.handle_mouse_event(mouse(MouseEventKind::ScrollDown, first))
        .unwrap();
    draw(&mut app, &mut terminal);
    let content = app
        .file_list
        .text_selection_area(Position::new(first.x + 1, first.y))
        .unwrap();
    let first_row: String = (content.x..content.right())
        .map(|column| terminal.backend().buffer()[(column, content.y)].symbol())
        .collect();
    assert!(
        first_row.starts_with("+line1"),
        "wheel moves the diff: {first_row:?}"
    );

    let seam = Position::new(app.changes_area.right(), app.changes_area.y + 4);
    app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), seam))
        .unwrap();
    assert_eq!(app.dragging_border, Some(1));
    let moved = Position::new(seam.x + 5, seam.y);
    app.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), moved))
        .unwrap();
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), moved))
        .unwrap();
    assert_ne!(app.border_frac, split);
    assert!(app.text_selection.origin().is_none());
    assert!(copies(&drain_actions(&mut app)).is_empty());
}

#[test]
fn graph_detail_border_drag_keeps_priority_over_text_selection() {
    let mut app = power_test_app();
    let _ = app.git_graph.set_commit_files(
        "a".repeat(40),
        "subject\nbody".into(),
        vec![("M".into(), "a.rs".into())],
    );
    app.git_graph.set_commit_diff("+diff".into());
    let mut terminal = TestTerminal::new(TestBackend::new(160, 60)).unwrap();
    draw(&mut app, &mut terminal);
    let message = find_text(&terminal, app.graph_area, "Message");
    let seam = Position::new(app.graph_area.x + 6, message.y);
    app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), seam))
        .unwrap();
    assert!(app.text_selection.origin().is_none());
    let moved = Position::new(seam.x, seam.y + 4);
    app.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), moved))
        .unwrap();
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), moved))
        .unwrap();
    draw(&mut app, &mut terminal);
    let after = find_text(&terminal, app.graph_area, "Message");
    assert_ne!(after.y, message.y, "the detail border moved");
    assert!(copies(&drain_actions(&mut app)).is_empty());
}

#[test]
fn graph_message_text_near_internal_borders_can_be_selected() {
    for width in [160, 90] {
        let mut app = power_test_app();
        let _ = app.git_graph.set_commit_files(
            "a".repeat(40),
            "subject\nbody".into(),
            vec![("M".into(), "a.rs".into())],
        );
        app.git_graph.set_commit_diff("+diff".into());
        let mut terminal = TestTerminal::new(TestBackend::new(width, 60)).unwrap();
        draw(&mut app, &mut terminal);
        let start = find_text(&terminal, app.graph_area, "subject");
        let end = Position::new(start.x + 6, start.y);
        start_drag(&mut app, &mut terminal, start, end);
        assert!(
            terminal.backend().buffer()[(start.x, start.y)]
                .modifier
                .contains(Modifier::REVERSED),
            "message text must be selectable at width {width}"
        );
        app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), end))
            .unwrap();
        assert_eq!(copies(&drain_actions(&mut app)), ["subject"]);
        draw(&mut app, &mut terminal);
        assert_eq!(find_text(&terminal, app.graph_area, "subject"), start);
    }
}

fn set_modal(app: &mut App, index: usize, visible: bool) {
    match index {
        0 => app.confirm_dialog.visible = visible,
        1 => app.path_input.visible = visible,
        2 => app.theme_picker.visible = visible,
        3 => app.graph_filter_picker.visible = visible,
        4 => app.graph_context_menu.visible = visible,
        5 => app.context_menu.visible = visible,
        6 => app.picker.visible = visible,
        7 => app.show_help = visible,
        _ => unreachable!(),
    }
}

#[test]
fn modals_cancel_text_and_border_grabs_without_leaking_the_release() {
    for overlay in 0..8 {
        let (mut app, mut terminal) = fixture("+alpha beta\n+gamma delta".into());
        let text = find_text(&terminal, app.changes_area, "+alpha beta");
        let start = Position::new(text.x + 1, text.y);
        let end = Position::new(text.x + 5, text.y);
        start_drag(&mut app, &mut terminal, start, end);
        app.dragging_border = Some(0);
        set_modal(&mut app, overlay, true);
        app.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), end))
            .unwrap();
        assert!(app.text_selection.origin().is_none(), "overlay {overlay}");
        assert!(app.dragging_border.is_none(), "overlay {overlay}");
        app.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), start))
            .unwrap();
        assert!(app.text_selection.origin().is_none(), "overlay {overlay}");
        set_modal(&mut app, overlay, false);
        app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), end))
            .unwrap();
        assert!(
            copies(&drain_actions(&mut app)).is_empty(),
            "overlay {overlay}"
        );
    }
}

#[test]
fn modal_keys_and_changed_content_cannot_copy_a_stale_selection() {
    let (mut app, mut terminal) = fixture("+alpha beta".into());
    let text = find_text(&terminal, app.changes_area, "+alpha beta");
    let start = Position::new(text.x + 1, text.y);
    let end = Position::new(text.x + 5, text.y);
    start_drag(&mut app, &mut terminal, start, end);
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), end))
        .unwrap();
    drain_actions(&mut app);
    app.confirm_dialog
        .show("Refresh?".into(), Action::RefreshAll);
    app.handle_key_event(KeyCode::Char('y').into()).unwrap();
    let actions = drain_actions(&mut app);
    assert!(copies(&actions).is_empty());
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::RefreshAll))
    );

    start_drag(&mut app, &mut terminal, start, end);
    app.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), end))
        .unwrap();
    drain_actions(&mut app);
    app.file_list.set_diff("+changed content".into());
    draw(&mut app, &mut terminal);
    assert!(app.text_selection.origin().is_none());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    let actions = drain_actions(&mut app);
    assert!(copies(&actions).is_empty());
    assert!(actions.iter().any(|action| matches!(action, Action::Quit)));
}
