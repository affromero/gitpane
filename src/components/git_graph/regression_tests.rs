use super::*;
use crate::components::Component;
use git2::{Oid, Repository, Signature};
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::mpsc::UnboundedReceiver;

fn commit(repo: &Repository, message: &str) -> Oid {
    let signature = Signature::now("Alice", "alice@example.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

async fn next_rows(rx: &mut UnboundedReceiver<Action>) -> (u64, Vec<GraphRow>) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match rx.recv().await.expect("graph action channel closed") {
                Action::GraphLoaded { generation, rows } => return (generation, rows),
                Action::GraphError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("graph did not finish loading")
}

fn row(id: u8, message: &str) -> GraphRow {
    let mut row = super::tests::mock_row("abcdef0", message, "Alice");
    row.oid = Oid::from_bytes(&[id; 20]).unwrap();
    row
}

#[test]
fn side_by_side_message_keeps_words_readable() {
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.load_repo("/repo".into(), "repo");
    let _ = graph.set_commit_files("abcdef0".into(), "Fix authentication".into(), vec![]);
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| graph.draw(frame, frame.area()).unwrap())
        .unwrap();
    let buffer = terminal.backend().buffer();
    let message = graph.msg_area;
    let rendered = (message.y + 1..message.bottom().saturating_sub(1))
        .map(|y| {
            (message.x + 1..message.right().saturating_sub(1))
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Fix"),
        "message loses readable words: {rendered:?}"
    );
}

#[test]
fn switching_repositories_rejects_pending_commit_details() {
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.load_repo("/first".into(), "first");
    graph.set_rows(vec![row(1, "first commit")]);
    assert!(graph.open_selected_commit_files().is_some());
    let pending_generation = graph.current_detail_generation();
    graph.load_repo("/second".into(), "second");
    if pending_generation == graph.current_detail_generation() {
        let _ = graph.set_commit_files("abcdef0".into(), "first commit".into(), vec![]);
    }
    assert!(
        !graph.has_detail(),
        "old repository detail reopened after switching"
    );
}

#[test]
fn closing_commit_details_rejects_pending_reopens() {
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.load_repo("/repo".into(), "repo");
    graph.set_rows(vec![row(1, "first commit")]);
    let _ = graph.set_commit_files("abcdef0".into(), "first commit".into(), vec![]);
    assert!(graph.open_selected_commit_files().is_some());
    let pending_generation = graph.current_detail_generation();
    graph.handle_key_event(KeyCode::Esc.into()).unwrap();
    if pending_generation == graph.current_detail_generation() {
        let _ = graph.set_commit_files("abcdef0".into(), "first commit".into(), vec![]);
    }
    assert!(
        !graph.has_detail(),
        "closed detail reopened from an older request"
    );
}

#[tokio::test]
async fn changing_filters_during_load_applies_the_latest_filter() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    commit(&repo, "Alice commit");
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.graph_options.show_stats = false;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    graph.register_action_handler(tx).unwrap();
    graph.load_repo(dir.path().into(), "repo");
    let (_, old_rows) = next_rows(&mut rx).await;
    graph.set_filters(GraphFilters {
        authors: Some(["Bob".into()].into()),
        ..Default::default()
    });
    graph.set_rows(old_rows);
    if graph.load_in_flight {
        let (_, rows) = next_rows(&mut rx).await;
        graph.set_rows(rows);
    }
    assert!(
        graph.selected_commit_menu_data().is_none(),
        "Bob filter retained Alice commit"
    );
    graph.load_repo("/other".into(), "other");
    graph.load_repo(dir.path().into(), "repo");
    assert!(
        graph.selected_commit_menu_data().is_none(),
        "cache restored unfiltered rows"
    );
}

#[tokio::test]
async fn refresh_requested_during_build_reads_new_commits() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    commit(&repo, "original");
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.graph_options.show_stats = false;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    graph.register_action_handler(tx).unwrap();
    graph.load_repo(dir.path().into(), "repo");
    let (_, old_rows) = next_rows(&mut rx).await;
    commit(&repo, "new commit");
    graph.force_reload_repo(dir.path().into(), "repo");
    graph.set_rows(old_rows);
    if graph.load_in_flight {
        let (_, rows) = next_rows(&mut rx).await;
        graph.set_rows(rows);
    }
    graph.open_search();
    for c in "new commit".chars() {
        graph.handle_search_key(KeyCode::Char(c).into()).unwrap();
    }
    graph.handle_search_key(KeyCode::Enter.into()).unwrap();
    assert_eq!(graph.selected_commit_menu_data().unwrap().2, "new commit");
}

#[test]
fn refreshing_search_keeps_the_matching_commit_selected() {
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.set_rows(vec![row(1, "old"), row(2, "needle")]);
    graph.open_search();
    for c in "needle".chars() {
        graph.handle_search_key(KeyCode::Char(c).into()).unwrap();
    }
    graph.handle_search_key(KeyCode::Enter.into()).unwrap();
    graph.set_rows(vec![row(3, "new"), row(1, "old"), row(2, "needle")]);
    assert_eq!(graph.selected_commit_menu_data().unwrap().2, "needle");
    graph.handle_key_event(KeyCode::Char('n').into()).unwrap();
    assert_eq!(graph.selected_commit_menu_data().unwrap().2, "needle");
}

#[tokio::test]
async fn refresh_requested_during_failed_build_retries_the_repository() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = GitGraph::new(Arc::new(Theme::default()));
    graph.graph_options.show_stats = false;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    graph.register_action_handler(tx).unwrap();
    graph.load_repo(dir.path().into(), "repo");
    let action = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let Action::GraphError { message, .. } = action else {
        panic!("an uninitialized repository should report a graph error");
    };
    let repo = Repository::init(dir.path()).unwrap();
    commit(&repo, "repository available");
    graph.force_reload_repo(dir.path().into(), "repo");
    graph.set_error(message);
    let (_, rows) = next_rows(&mut rx).await;
    graph.set_rows(rows);
    assert_eq!(
        graph.selected_commit_menu_data().unwrap().2,
        "repository available"
    );
}
