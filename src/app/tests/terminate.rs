//! A terminating signal must leave the terminal usable. The run loop restores
//! it through `Tui::exit()`, so the signal has to reach the ordinary quit path
//! rather than ending the process where it stands.

use super::*;

/// A signal quits at once: nobody is left to press `q` again, and holding the
/// exit open for an in-flight git operation keeps the terminal in raw mode.
#[tokio::test]
async fn terminate_forces_the_ordinary_quit_path() {
    let _count = GIT_OP_COUNT.lock().await;
    let mut app = power_test_app();
    let (tx, _rx) = mpsc::unbounded_channel();
    let guard = GitOpGuard::new(RepoId(std::path::PathBuf::from("/tmp/repo")), tx);
    // An in-flight operation would otherwise hold the exit open.
    assert!(!app.ready_to_exit());

    app.handle_event(Event::Terminate).unwrap();

    assert!(app.should_quit);
    assert!(app.force_quit);
    // No second press to ask for, so the loop can restore the terminal on its
    // very next pass.
    assert!(app.ready_to_exit());
    guard.complete();
}
