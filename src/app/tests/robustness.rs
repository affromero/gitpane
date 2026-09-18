use super::*;

#[tokio::test]
async fn removing_one_repository_preserves_namesakes_and_prefix_siblings() {
    let tmp = tempfile::TempDir::new().unwrap();
    let removed = make_repo(tmp.path(), "a/repo").canonicalize().unwrap();
    let namesake = make_repo(tmp.path(), "b/repo").canonicalize().unwrap();
    let sibling = make_repo(tmp.path(), "a/repo-copy").canonicalize().unwrap();
    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 3,
        write_target_override: Some(tmp.path().join("config.toml")),
        ..Config::default()
    };
    let mut app = App::new(config);
    assert_eq!(app.repo_list.repos.len(), 3);
    app.handle_repo_admin(Action::RemoveRepo(RepoId(removed.clone())))
        .unwrap();
    app.handle_repo_admin(Action::DiscoverNewRepos).unwrap();
    let paths: Vec<_> = app.repo_list.repos.iter().map(|r| r.path.clone()).collect();
    assert!(!paths.contains(&removed));
    assert!(paths.contains(&namesake));
    assert!(paths.contains(&sibling));
}

#[test]
fn leaving_graph_with_escape_rejects_pending_commit_details() {
    let mut app = power_test_app();
    app.focus = FocusPanel::Graph;
    let generation = app.git_graph.current_detail_generation();
    app.handle_key_event(KeyCode::Esc.into()).unwrap();
    app.handle_action_rest(Action::CommitFilesLoaded {
        generation,
        oid: "abc1234".into(),
        message: "late commit".into(),
        files: vec![("M".into(), "late.txt".into())],
    })
    .unwrap();
    assert!(!app.git_graph.has_detail());
    assert_eq!(app.focus, FocusPanel::Changes);
}

#[test]
fn error_messages_preserve_unicode_and_truncate_by_characters() {
    let mut app = power_test_app();
    for message in ["é".repeat(61), "界".repeat(121), "🙂\n".repeat(80)] {
        app.handle_action_rest(Action::Error(message.clone()))
            .unwrap();
        let displayed = &app.error_message.as_ref().unwrap().0;
        assert!(!displayed.contains('\n'));
        assert!(displayed.chars().count() <= 120);
        if message.chars().count() <= 120 {
            assert_eq!(*displayed, message);
        } else {
            assert!(displayed.ends_with("..."));
        }
    }
}

#[test]
fn panels_render_and_drag_after_shrinking_terminal() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut app = power_test_app();
    app.config.github.enabled = true;
    for show in [false, true] {
        app.github_forced = Some(show);
        for width in [0, 1, 3, 20, 80, 99, 100, 160] {
            for height in 0..=22 {
                app.border_frac = [0.3, 0.8, 0.9];
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|frame| app.draw(frame).unwrap()).unwrap();
                for border in 0..if show { 3 } else { 2 } {
                    app.dragging_border = Some(border);
                    app.handle_mouse_event(MouseEvent {
                        kind: MouseEventKind::Drag(MouseButton::Left),
                        column: width / 2,
                        row: height / 2,
                        modifiers: KeyModifiers::NONE,
                    })
                    .unwrap();
                    terminal.draw(|frame| app.draw(frame).unwrap()).unwrap();
                    assert!(app.border_frac.iter().all(|v| v.is_finite()));
                    assert!(app.border_frac[0] <= app.border_frac[1]);
                    if show {
                        assert!(app.border_frac[1] <= app.border_frac[2]);
                    }
                }
            }
        }
    }
}

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Draw the app and return the whole frame as text.
fn frame_text(
    app: &mut App,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) -> String {
    terminal.draw(|frame| app.draw(frame).unwrap()).unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|col| buffer[(col, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// The `seen/total` counter of the pane showing `total` rows, from frame text.
fn counter_for(frame: &str, total: usize) -> String {
    frame
        .lines()
        .find(|line| line.contains(&format!("/{total}")))
        .unwrap_or_else(|| panic!("no counter for {total} rows in:\n{frame}"))
        .trim()
        .to_owned()
}

#[test]
fn a_scroll_indicator_press_outranks_the_panel_seam_next_to_it() {
    // The changes panel's diff pane ends two cells left of the changes|graph
    // seam, i.e. inside the seam's ±2-cell grab zone. Losing the grab to the seam
    // would resize panels on the drag instead of scrubbing the diff.
    let mut app = power_test_app();
    app.file_list.set_diff(
        (0..60)
            .map(|i| format!("+line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    let _ = frame_text(&mut app, &mut terminal);

    let seam = app.changes_area.x + app.changes_area.width;
    let column = seam - 2; // the diff pane's indicator column
    let bottom = app.changes_area.y + app.changes_area.height - 2;
    let split = app.border_frac;
    let event = |kind, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };

    app.handle_mouse_event(event(MouseEventKind::Down(MouseButton::Left), bottom))
        .unwrap();
    assert!(
        app.dragging_border.is_none(),
        "the indicator must take the grab from the seam"
    );
    // The press still reached the panel: the diff jumped to its end.
    let frame = frame_text(&mut app, &mut terminal);
    assert!(
        counter_for(&frame, 60).contains("60/60"),
        "the press must scrub the diff, not just arm a resize: {:?}",
        counter_for(&frame, 60)
    );

    // A drag scrubs the diff and leaves the panel split alone.
    app.handle_mouse_event(event(
        MouseEventKind::Drag(MouseButton::Left),
        app.changes_area.y + 1,
    ))
    .unwrap();
    assert_eq!(app.border_frac, split, "a scrub must not resize panels");
    let frame = frame_text(&mut app, &mut terminal);
    let counter = counter_for(&frame, 60);
    assert!(
        !counter.contains("60/60"),
        "the drag must scrub the diff away from its end: {counter:?}"
    );

    // Releasing hands the grab back: a later drag without a press does nothing.
    app.handle_mouse_event(event(MouseEventKind::Up(MouseButton::Left), bottom))
        .unwrap();
    assert!(app.dragging_border.is_none());
    app.handle_mouse_event(event(MouseEventKind::Drag(MouseButton::Left), bottom))
        .unwrap();
    let frame = frame_text(&mut app, &mut terminal);
    assert!(
        !counter_for(&frame, 60).contains("60/60"),
        "a drag without a press must not scrub"
    );
}

#[test]
fn a_press_ends_a_grab_whose_release_went_to_another_panel() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    // Events route by pointer position, so a release over a different panel never
    // reaches the panel that started the drag. The next press has to end it, or a
    // drag that later wanders back in would keep scrubbing.
    let mut app = power_test_app();
    let _ = app.git_graph.set_commit_files(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        "message".to_string(),
        vec![("M".into(), "a.rs".into())],
    );
    app.git_graph.set_commit_diff(
        (0..60)
            .map(|i| format!("+line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    let _ = frame_text(&mut app, &mut terminal);

    // The graph panel's diff pane is the bottom slice of it; stacked panes share
    // the indicator's column, so only the row picks the pane.
    let column = app.graph_area.x + app.graph_area.width - 2;
    let bottom = app.graph_area.y + app.graph_area.height - 2;
    let event = |kind, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_event(event(MouseEventKind::Down(MouseButton::Left), bottom))
        .unwrap();
    let frame = frame_text(&mut app, &mut terminal);
    assert!(
        counter_for(&frame, 60).contains("60/60"),
        "the press grabbed the diff indicator"
    );

    // Release over the changes panel, clear of the panel seams so no resize grab
    // is armed: the graph never sees this release.
    let elsewhere = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: app.changes_area.x + 6,
        row: app.changes_area.y + 2,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_event(elsewhere).unwrap();
    // A fresh press in the changes panel, then a drag back inside the graph panel.
    app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: app.changes_area.x + 6,
        row: app.changes_area.y + 2,
        modifiers: KeyModifiers::NONE,
    })
    .unwrap();
    // Away from the grabbed row, so a stale grab would visibly move the counter.
    app.handle_mouse_event(event(MouseEventKind::Drag(MouseButton::Left), bottom - 4))
        .unwrap();
    let frame = frame_text(&mut app, &mut terminal);
    assert!(
        counter_for(&frame, 60).contains("60/60"),
        "a press elsewhere must end the stale grab: {:?}",
        counter_for(&frame, 60)
    );
}
