use super::*;
use crate::components::file_list::StashFileEntry;
use crate::git::stash::{StashFile, StashSource};

fn stash_file(path: &str) -> StashFileEntry {
    stash_file_from(path, StashSource::Tracked)
}

fn stash_file_from(path: &str, source: StashSource) -> StashFileEntry {
    StashFileEntry::from_stash_file(StashFile {
        source,
        status: "M".to_string(),
        path: path.to_string(),
        old_path: None,
        new_path: Some(path.to_string()),
        is_binary: false,
        is_submodule: false,
    })
}

fn app_with_repo() -> (tempfile::TempDir, App, RepoId) {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = make_repo(tmp.path(), "repo");
    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        write_target_override: Some(tmp.path().join("config.toml")),
        scan_depth: 2,
        ..Config::default()
    };
    let app = App::new(config).unwrap();
    (tmp, app, RepoId(repo.canonicalize().unwrap()))
}

#[test]
fn a_late_stash_load_cannot_replace_a_newer_source() {
    let (_tmp, mut app, id) = app_with_repo();
    let generation = app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        "a".repeat(40),
        "stash@{0}".into(),
        "first".into(),
    );
    app.file_list.set_stash_files(vec![stash_file("a.txt")]);
    assert_eq!(app.file_list.selected_path().as_deref(), Some("a.txt"));

    // A result for an older/other snapshot arrives after the switch.
    app.handle_action_rest(Action::StashFilesLoaded {
        repo_id: id,
        oid: "b".repeat(40),
        generation,
        result: Ok(vec![stash_file("stale.txt")]),
    })
    .unwrap();

    assert_eq!(app.file_list.selected_path().as_deref(), Some("a.txt"));
    assert!(app.file_list.is_stash());
}

#[test]
fn stash_mode_refuses_workspace_write_actions() {
    let (_tmp, mut app, id) = app_with_repo();
    app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        "c".repeat(40),
        "stash@{0}".into(),
        "read only".into(),
    );

    app.handle_action_rest(Action::StageFile(
        id.clone(),
        std::path::PathBuf::from("a.txt"),
    ))
    .unwrap();

    let actions = drain_actions(&mut app);
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::Error(message) if message.contains("read-only")
    )));
    assert!(
        actions
            .iter()
            .all(|action| !matches!(action, Action::RefreshRepo(_))),
        "a refused write must not schedule a git operation: {actions:?}"
    );
}

#[test]
fn exit_stash_view_restores_the_selected_repo_working_tree() {
    let (_tmp, mut app, id) = app_with_repo();
    let idx = app.repo_list.resolve_index(&id).expect("repo discovered");
    let mut status = test_status("main", None);
    status.files = vec![crate::git::status::FileEntry {
        path: "work.txt".into(),
        status: crate::git::status::FileStatus::Modified,
        staged: false,
        unstaged: true,
        is_submodule: false,
        submodule_state: None,
        submodule_warn: crate::git::status::SubmoduleWarn::default(),
        submodule_head: None,
    }];
    app.repo_list.update_status(idx, status);
    app.file_list.show_stash_loading(
        id,
        "repo".into(),
        "d".repeat(40),
        "stash@{0}".into(),
        "preview".into(),
    );

    app.exit_stash_view().unwrap();

    assert!(!app.file_list.is_stash());
    assert_eq!(app.file_list.selected_path().as_deref(), Some("work.txt"));
}

#[test]
fn global_escape_leaves_stash_before_quitting() {
    let (_tmp, mut app, id) = app_with_repo();
    app.file_list.show_stash_loading(
        id,
        "repo".into(),
        "e".repeat(40),
        "stash@{0}".into(),
        "preview".into(),
    );
    app.focus = FocusPanel::Repos;
    drain_actions(&mut app);

    app.handle_key_event(KeyCode::Esc.into()).unwrap();

    let actions = drain_actions(&mut app);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::ExitStashView)),
        "Esc must leave the stash preview: {actions:?}"
    );
    assert!(
        actions.iter().all(|action| !matches!(action, Action::Quit)),
        "Esc must not quit while a stash preview is open: {actions:?}"
    );
}

#[test]
fn escape_closes_a_stash_diff_before_leaving_the_preview() {
    let (_tmp, mut app, id) = app_with_repo();
    app.file_list.show_stash_loading(
        id,
        "repo".into(),
        "f".repeat(40),
        "stash@{0}".into(),
        "preview".into(),
    );
    app.file_list.set_stash_files(vec![stash_file("a.txt")]);
    app.file_list.set_diff("diff".into());
    app.focus = FocusPanel::Changes;
    drain_actions(&mut app);

    app.handle_key_event(KeyCode::Esc.into()).unwrap();

    assert!(!app.file_list.viewing_diff());
    assert!(
        drain_actions(&mut app)
            .iter()
            .all(|action| !matches!(action, Action::ExitStashView)),
        "the first Esc only closes the diff"
    );
}

#[test]
fn status_refresh_renumbers_the_label_or_drops_a_deleted_stash() {
    let (_tmp, mut app, id) = app_with_repo();
    let idx = app.repo_list.resolve_index(&id).expect("repo discovered");
    let oid = "9".repeat(40);
    app.file_list.show_stash_loading(
        id,
        "repo".into(),
        oid.clone(),
        "stash@{0}".into(),
        "old summary".into(),
    );

    let mut renumbered = test_status("main", None);
    renumbered.stashes = vec![
        crate::git::status::StashEntry {
            index: 0,
            message: "other".into(),
            oid: "8".repeat(40),
        },
        crate::git::status::StashEntry {
            index: 1,
            message: "renumbered".into(),
            oid: oid.clone(),
        },
    ];
    app.repo_list.update_status(idx, renumbered);
    app.reconcile_stash_preview();
    let stash = app.file_list.stash_view().expect("still previewing by OID");
    assert_eq!(stash.oid, oid);
    assert_eq!(stash.label, "stash@{1}");
    assert_eq!(stash.summary, "renumbered");

    let mut deleted = test_status("main", None);
    deleted.stashes = Vec::new();
    app.repo_list.update_status(idx, deleted);
    app.reconcile_stash_preview();
    assert!(
        !app.file_list.is_stash(),
        "a deleted stash must fall back to the working tree"
    );
}

#[test]
fn review_tui_old_stash_error_after_reentering_same_snapshot() {
    let (_tmp, mut app, id) = app_with_repo();
    let oid = "a".repeat(40);
    let first = app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        oid.clone(),
        "stash@{0}".into(),
        "first".into(),
    );

    // A → B → A: the same snapshot is selected again under a new request.
    app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        "b".repeat(40),
        "stash@{1}".into(),
        "other".into(),
    );
    let second = app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        oid.clone(),
        "stash@{0}".into(),
        "again".into(),
    );
    assert_ne!(first, second);

    app.handle_action_rest(Action::StashFilesLoaded {
        repo_id: id.clone(),
        oid: oid.clone(),
        generation: second,
        result: Ok(vec![stash_file("new.txt")]),
    })
    .unwrap();
    assert_eq!(app.file_list.selected_path().as_deref(), Some("new.txt"));

    // The first visit's late failure names the same snapshot and must still
    // be dropped instead of clearing the second visit's preview.
    app.handle_action_rest(Action::StashFilesLoaded {
        repo_id: id,
        oid,
        generation: first,
        result: Err("old failure".into()),
    })
    .unwrap();

    assert_eq!(app.file_list.selected_path().as_deref(), Some("new.txt"));
}

#[test]
fn review_tui_refresh_retains_stash_file_origin() {
    let (_tmp, mut app, id) = app_with_repo();
    let oid = "d".repeat(40);
    let generation = app.file_list.show_stash_loading(
        id.clone(),
        "repo".into(),
        oid.clone(),
        "stash@{0}".into(),
        "duplicate".into(),
    );
    let files = vec![
        stash_file_from("same.txt", StashSource::Tracked),
        stash_file_from("same.txt", StashSource::SavedUntracked),
    ];
    app.handle_action_rest(Action::StashFilesLoaded {
        repo_id: id.clone(),
        oid: oid.clone(),
        generation,
        result: Ok(files.clone()),
    })
    .unwrap();

    app.focus = FocusPanel::Changes;
    app.handle_key_event(KeyCode::Down.into()).unwrap();
    app.handle_key_event(KeyCode::Enter.into()).unwrap();
    let source = drain_actions(&mut app)
        .iter()
        .find_map(|action| match action {
            Action::ShowStashDiff { source, path, .. } => Some((*source, path.clone())),
            _ => None,
        })
        .expect("opening the saved-untracked row requests its diff");
    assert_eq!(
        source,
        (
            StashSource::SavedUntracked,
            std::path::PathBuf::from("same.txt")
        )
    );

    // Reinstalling the rows must refresh the row that is open, not the first
    // row that happens to share its path.
    app.file_list.set_stash_files(files);
    match app.file_list.refresh_diff() {
        Some(Action::ShowStashDiff { source, path, .. }) => {
            assert_eq!(source, StashSource::SavedUntracked);
            assert_eq!(path, std::path::PathBuf::from("same.txt"));
        }
        other => panic!("expected the saved-untracked diff, got {other:?}"),
    }
}
