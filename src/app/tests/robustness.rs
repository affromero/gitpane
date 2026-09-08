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
