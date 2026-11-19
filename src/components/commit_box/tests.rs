use super::*;
use ratatui::{Terminal, backend::TestBackend};
use std::path::PathBuf;

fn id(name: &str) -> RepoId {
    RepoId(PathBuf::from(format!("/repos/{name}")))
}

fn open(staged: usize) -> CommitBox {
    let mut commit_box = CommitBox::new(Arc::new(Theme::default()));
    commit_box.show(id("app"), "app", staged);
    commit_box
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn press(commit_box: &mut CommitBox, event: KeyEvent) -> Option<Action> {
    commit_box.handle_key_event(event).unwrap()
}

fn type_text(commit_box: &mut CommitBox, text: &str) {
    for c in text.chars() {
        assert!(press(commit_box, key(KeyCode::Char(c))).is_none());
    }
}

fn committed_message(action: Option<Action>) -> String {
    match action {
        Some(Action::Commit {
            id: target,
            message,
            stage_all: false,
        }) if target == id("app") => message,
        other => panic!("expected a commit for app, got {other:?}"),
    }
}

fn screen(commit_box: &CommitBox) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal
        .draw(|frame| commit_box.draw(frame, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn enter_commits_from_the_summary_and_adds_lines_in_the_description() {
    let mut commit_box = open(1);
    type_text(&mut commit_box, "Fix parser");
    press(&mut commit_box, key(KeyCode::Tab));
    type_text(&mut commit_box, "It dropped quotes.");
    assert!(press(&mut commit_box, key(KeyCode::Enter)).is_none());
    type_text(&mut commit_box, "Now it keeps them.");
    press(&mut commit_box, key(KeyCode::BackTab));

    assert_eq!(
        committed_message(press(&mut commit_box, key(KeyCode::Enter))),
        "Fix parser\n\nIt dropped quotes.\nNow it keeps them."
    );
}

#[test]
fn ctrl_s_commits_from_the_description() {
    let mut commit_box = open(1);
    type_text(&mut commit_box, "Bump version");
    press(&mut commit_box, key(KeyCode::Tab));

    assert_eq!(
        committed_message(press(&mut commit_box, ctrl('s'))),
        "Bump version"
    );
}

#[test]
fn the_summary_stays_on_one_line() {
    let mut commit_box = open(1);
    type_text(&mut commit_box, "One");
    press(&mut commit_box, ctrl('m'));
    type_text(&mut commit_box, " line");

    assert_eq!(commit_box.summary.lines(), ["One line"]);
}

#[test]
fn a_blank_summary_does_not_commit() {
    let mut commit_box = open(1);
    press(&mut commit_box, key(KeyCode::Tab));
    type_text(&mut commit_box, "Only a description");

    assert!(press(&mut commit_box, ctrl('s')).is_none());
    assert!(commit_box.error.is_some());
}

#[test]
fn esc_keeps_the_text_for_that_checkout_only() {
    let mut commit_box = open(1);
    type_text(&mut commit_box, "Half written");
    press(&mut commit_box, key(KeyCode::Esc));
    assert!(!commit_box.visible);

    commit_box.show(id("other"), "other", 1);
    assert_eq!(commit_box.summary_text(), "");
    press(&mut commit_box, key(KeyCode::Esc));

    commit_box.show(id("app"), "app", 1);
    type_text(&mut commit_box, " message");
    assert_eq!(
        committed_message(press(&mut commit_box, key(KeyCode::Enter))),
        "Half written message"
    );
}

#[test]
fn a_failed_commit_keeps_the_text_and_a_landed_one_clears_it() {
    let mut commit_box = open(1);
    type_text(&mut commit_box, "Add feature");
    assert!(press(&mut commit_box, key(KeyCode::Enter)).is_some());
    commit_box.set_committing();
    type_text(&mut commit_box, "ignored while committing");

    assert!(!commit_box.failed(&id("other"), "not this box"));
    assert!(commit_box.failed(&id("app"), "pre-commit: lint failed\n"));
    assert_eq!(commit_box.error.as_deref(), Some("pre-commit: lint failed"));
    assert_eq!(commit_box.summary_text(), "Add feature");

    commit_box.set_committing();
    commit_box.committed(&id("app"));
    assert!(!commit_box.visible);
    commit_box.show(id("app"), "app", 0);
    assert_eq!(commit_box.summary_text(), "");
}

#[test]
fn pasting_a_whole_message_into_the_summary_splits_it() {
    let mut commit_box = open(1);
    commit_box.paste("Fix typo\r\n\r\nIn the README.\r\nAnd the docs.");

    assert_eq!(commit_box.summary_text(), "Fix typo");
    assert_eq!(
        committed_message(press(&mut commit_box, ctrl('s'))),
        "Fix typo\n\nIn the README.\nAnd the docs."
    );
}

#[test]
fn the_box_shows_what_will_be_committed_and_the_keys() {
    let text = screen(&open(0));
    assert!(text.contains("Commit app · nothing staged"), "{text}");
    assert!(text.contains("Enter commit"), "{text}");
    assert!(!text.contains("Ctrl+G"), "{text}");

    let mut commit_box = open(2);
    let text = screen(&commit_box);
    assert!(text.contains("2 staged files"), "{text}");

    commit_box.failed(&id("app"), "hook: rejected\nsecond line");
    let text = screen(&commit_box);
    assert!(text.contains("✗ hook: rejected"), "{text}");
    assert!(text.contains("second line"), "{text}");
}
