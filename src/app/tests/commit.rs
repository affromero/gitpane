use super::*;
use crate::git::status::{FileEntry, FileStatus, SubmoduleWarn};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

fn run(path: &Path, args: &[&str]) -> String {
    let out = crate::git::process::git_command(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo with one commit and `a.txt` modified (staged or not), listed in an
/// app whose Changes panel shows it. The repo has its own identity and hooks
/// directory, so global Git config can't leak in.
fn fixture(staged: bool) -> (tempfile::TempDir, App, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("repo");
    std::fs::create_dir(&path).unwrap();
    run(&path, &["init", "-q"]);
    run(&path, &["config", "user.name", "Test"]);
    run(&path, &["config", "user.email", "test@example.com"]);
    run(&path, &["config", "commit.gpgsign", "false"]);
    run(&path, &["config", "core.hooksPath", "hooks"]);
    std::fs::create_dir(path.join("hooks")).unwrap();
    std::fs::write(path.join(".gitignore"), "hooks/\n").unwrap();
    std::fs::write(path.join("a.txt"), "a\n").unwrap();
    run(&path, &["add", "--all"]);
    run(&path, &["commit", "-q", "-m", "initial"]);
    std::fs::write(path.join("a.txt"), "a changed\n").unwrap();
    if staged {
        run(&path, &["add", "a.txt"]);
    }
    let path = path.canonicalize().unwrap();

    let mut config = Config {
        write_target_override: Some(tmp.path().join("config.toml")),
        root_dirs: vec![],
        pinned_repos: vec![path.clone()],
        ..Config::default()
    };
    config.ui.show_liveness = false;
    let mut app = App::new(config).unwrap();
    let entry = FileEntry {
        path: "a.txt".into(),
        status: FileStatus::Modified,
        staged,
        unstaged: !staged,
        is_submodule: false,
        submodule_state: None,
        submodule_warn: SubmoduleWarn::default(),
        submodule_head: None,
    };
    app.file_list
        .set_files(vec![entry], "repo", RepoId(path.clone()));
    (tmp, app, path)
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_key_event(KeyEvent::new(code, modifiers))
        .unwrap();
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// Hand the next queued action back to the app, as the run loop would.
fn dispatch(app: &mut App) -> Action {
    let action = app.action_rx.try_recv().expect("no action queued");
    app.handle_action_rest(action.clone()).unwrap();
    action
}

fn open_box(app: &mut App) {
    app.focus = FocusPanel::Changes;
    press(app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(matches!(dispatch(app), Action::OpenCommitBox));
    assert!(app.commit_box.visible);
}

/// Feed the commit box's actions back until `done` matches one. Status
/// refreshes and toasts are skipped: they need the live terminal loop.
async fn run_until(app: &mut App, done: impl Fn(&Action) -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let action = app.action_rx.recv().await.unwrap();
            let finished = done(&action);
            if matches!(
                action,
                Action::Commit { .. } | Action::CommitFinished { .. }
            ) {
                app.handle_action_rest(action).unwrap();
            }
            if finished {
                break;
            }
        }
    })
    .await
    .expect("the commit box never got its result");
}

#[test]
fn c_opens_the_box_from_the_repo_and_changes_panels_only() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_tmp, mut app, path) = fixture(true);
    for focus in [FocusPanel::Repos, FocusPanel::Changes] {
        app.focus = focus;
        press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(matches!(dispatch(&mut app), Action::OpenCommitBox));
        assert_eq!(app.commit_box.target_id(), Some(&RepoId(path.clone())));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.commit_box.visible);
    }

    app.focus = FocusPanel::Graph;
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    while let Ok(action) = app.action_rx.try_recv() {
        assert!(!matches!(action, Action::OpenCommitBox));
    }
    assert!(!app.commit_box.visible);
}

#[tokio::test]
async fn nothing_staged_asks_first_then_stages_everything() {
    if !crate::git::git_test_available() {
        return;
    }
    let _count = GIT_OP_COUNT.lock().await;
    let (_tmp, mut app, path) = fixture(false);
    open_box(&mut app);
    type_text(&mut app, "Change a");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        dispatch(&mut app),
        Action::Commit {
            stage_all: false,
            ..
        }
    ));
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;
    assert!(app.confirm_dialog.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "initial\n");

    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;

    assert!(!app.commit_box.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "Change a\n");
    assert_eq!(run(&path, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn commits_only_the_index_when_a_file_was_staged_after_opening() {
    if !crate::git::git_test_available() {
        return;
    }
    let _count = GIT_OP_COUNT.lock().await;
    let (_tmp, mut app, path) = fixture(false);
    std::fs::write(path.join("extra.txt"), "leave untracked\n").unwrap();
    open_box(&mut app);
    run(&path, &["add", "a.txt"]);

    type_text(&mut app, "Only staged");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;

    assert!(!app.confirm_dialog.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "Only staged\n");
    assert_eq!(run(&path, &["status", "--short"]), "?? extra.txt\n");
}

#[tokio::test]
async fn asks_before_staging_when_external_git_empties_the_index() {
    if !crate::git::git_test_available() {
        return;
    }
    let _count = GIT_OP_COUNT.lock().await;
    let (_tmp, mut app, path) = fixture(true);
    open_box(&mut app);
    run(&path, &["reset", "-q", "--", "a.txt"]);

    type_text(&mut app, "Change a");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;

    assert!(app.confirm_dialog.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "initial\n");
    assert!(run(&path, &["diff", "--cached", "--name-only"]).is_empty());
}

#[tokio::test]
async fn rejects_stage_all_when_the_index_changes_during_confirmation() {
    if !crate::git::git_test_available() {
        return;
    }
    let _count = GIT_OP_COUNT.lock().await;
    let (_tmp, mut app, path) = fixture(false);
    std::fs::write(path.join("extra.txt"), "leave untracked\n").unwrap();
    open_box(&mut app);
    type_text(&mut app, "Change a");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;
    assert!(app.confirm_dialog.visible);

    run(&path, &["add", "a.txt"]);
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { .. })
    })
    .await;

    assert!(app.commit_box.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "initial\n");
    assert_eq!(
        run(&path, &["status", "--short"]),
        "M  a.txt\n?? extra.txt\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_rejecting_hook_keeps_the_box_and_the_message() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::git::git_test_available() {
        return;
    }
    let _count = GIT_OP_COUNT.lock().await;
    let (_tmp, mut app, path) = fixture(true);
    let hook = path.join("hooks").join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint failed' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    open_box(&mut app);
    type_text(&mut app, "Change a");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    run_until(&mut app, |action| {
        matches!(action, Action::CommitFinished { result: Err(_), .. })
    })
    .await;

    assert!(app.commit_box.visible);
    assert_eq!(run(&path, &["log", "-1", "--format=%s"]), "initial\n");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        app.action_rx.try_recv(),
        Ok(Action::Commit { message, .. }) if message == "Change a"
    ));
}

#[test]
fn a_stash_preview_refuses_to_open_the_box() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_tmp, mut app, path) = fixture(true);
    app.file_list.show_stash_loading(
        RepoId(path),
        "repo".into(),
        "a".repeat(40),
        "stash@{0}".into(),
        "wip".into(),
    );
    app.focus = FocusPanel::Changes;
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    dispatch(&mut app);

    assert!(!app.commit_box.visible);
    assert!(matches!(
        app.action_rx.try_recv(),
        Ok(Action::Error(error)) if error.contains("read-only")
    ));
}
