use super::*;

fn file_open_app() -> (tempfile::TempDir, App, RepoId, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    git2::Repository::init(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    let file = repo.join("source file.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let mut config = Config {
        root_dirs: vec![temp.path().to_path_buf()],
        ..Config::default()
    };
    config.open.command = Some("git hash-object {path}".into());
    config.open.placement = "command".into();
    (temp, App::new(config).unwrap(), RepoId(repo), file)
}

fn run_file_open(request: PendingLaunch, expected: &std::path::Path) {
    let plan = crate::session::launcher::plan_with_target(
        request.command.as_deref(),
        "command",
        &request.dir.to_string_lossy(),
        &request.target.to_string_lossy(),
        request.base.as_deref(),
        crate::session::env::Multiplexer::None,
    );
    let crate::session::launcher::LaunchPlan::Spawn(argv) = plan else {
        panic!("expected an argv launch")
    };
    let mut command = std::process::Command::new(&argv[0]);
    crate::git::process::clear_inherited_git_env(&mut command);
    let output = command
        .args(&argv[1..])
        .current_dir(&request.dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_oid =
        git2::Oid::hash_object(git2::ObjectType::Blob, &std::fs::read(expected).unwrap()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected_oid.to_string()
    );
    assert_eq!(request.dir, expected.parent().unwrap());
}

#[test]
fn configured_file_open_passes_file_to_editor_with_directory_cwd() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, app, id, file) = file_open_app();
    let request = app
        .file_open_request(&id, std::path::Path::new("source file.rs"))
        .unwrap();
    run_file_open(request, &file);
}

#[test]
fn placement_picker_preserves_file_target_and_directory_cwd() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, mut app, id, file) = file_open_app();
    // Herdr choices are local data, so no real multiplexer is needed.
    app.mux = crate::session::env::Multiplexer::Herdr;
    let request = app
        .file_open_request(&id, std::path::Path::new("source file.rs"))
        .unwrap();
    app.start_placement_picker(request);
    let Some(PendingPick::Launch(request)) = app.pending_pick.take() else {
        panic!("expected a parked file launch")
    };
    run_file_open(request, &file);
}

#[tokio::test]
async fn herdr_placement_query_keeps_picker_responsive_and_ignores_cancelled_results() {
    let mut app = App::new(Config::default()).unwrap();
    let request = || PendingLaunch {
        dir: "/tmp".into(),
        target: "/tmp".into(),
        command: None,
        base: None,
        label: "open",
    };
    let (release, wait) = std::sync::mpsc::channel();
    app.start_herdr_placement_picker(request(), move || {
        wait.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        Ok(vec![("Old".into(), "old".into())])
    });
    assert!(app.picker.visible);
    assert!(matches!(
        app.picker
            .handle_key_event(crossterm::event::KeyCode::Esc.into())
            .unwrap(),
        Some(Action::PickerCancel)
    ));
    app.picker.hide();
    app.pending_pick = None;
    app.start_herdr_placement_picker(request(), || Ok(vec![("New".into(), "new".into())]));
    release.send(()).unwrap();
    for _ in 0..2 {
        let action = tokio::time::timeout(std::time::Duration::from_secs(5), app.action_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let Action::PlacementChoicesLoaded { generation, result } = action else {
            panic!("unexpected action: {action:?}");
        };
        app.finish_placement_picker(generation, result);
    }
    assert!(matches!(
        app.picker.handle_key_event(crossterm::event::KeyCode::Enter.into()).unwrap(),
        Some(Action::PickerChose(value)) if value == "new"
    ));
}

#[test]
fn herdr_placement_failure_closes_picker_and_reports_error() {
    let mut app = App::new(Config::default()).unwrap();
    app.pending_pick = Some(PendingPick::Launch(PendingLaunch {
        dir: "/tmp".into(),
        target: "/tmp".into(),
        command: None,
        base: None,
        label: "open",
    }));
    app.picker.show("Loading placements...", Vec::new());
    app.finish_placement_picker(0, Err("desktop unavailable".into()));
    assert!(!app.picker.visible);
    assert!(app.pending_pick.is_none());
    assert!(
        matches!(app.action_rx.try_recv(), Ok(Action::Error(error)) if error == "desktop unavailable")
    );
}

#[test]
fn sanitize_stdout_truncates_and_escapes_controls() {
    let long = "x".repeat(300);
    let s = sanitize_stdout(&long);
    assert_eq!(s.chars().count(), 201);
    assert!(s.ends_with('…'));

    let s = sanitize_stdout("a\nb\r\tc\u{1b}[31m");
    assert_eq!(s, "a b  c?[31m");
}
