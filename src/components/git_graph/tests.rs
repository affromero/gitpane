use super::*;
use crate::components::Component;
use crate::git::graph::{BranchLabel, GraphRow, LaneSegment};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use git2::Oid;
use ratatui::layout::Rect;

fn mock_row(short_id: &str, message: &str, author: &str) -> GraphRow {
    GraphRow {
        commit_col: 0,
        lanes: vec![LaneSegment::Commit],
        horizontal_spans: Vec::new(),
        oid: Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        short_id: short_id.to_string(),
        message: message.to_string(),
        author: author.to_string(),
        time: 0,
        labels: Vec::new(),
        is_merge: false,
        parent_oids: Vec::new(),
        diff_stat: None,
        collapsed: None,
    }
}

#[test]
fn right_clicking_a_graph_row_opens_its_context_menu() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.graph_list_area = Rect::new(0, 0, 80, 4);

    let action = graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

    assert!(matches!(action, Some(Action::OpenGraphContextMenu)));
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn test_search_matches_message() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        mock_row("abc1234", "fix: resolve crash", "Alice"),
        mock_row("def5678", "feat: add login", "Bob"),
        mock_row("ghi9012", "chore: update deps", "Alice"),
    ]);

    graph.search.input = "login".to_string();
    graph.update_search_matches();

    assert_eq!(graph.search.matches, vec![1]);
}

#[test]
fn test_search_matches_author() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        mock_row("abc1234", "first", "Alice"),
        mock_row("def5678", "second", "Bob"),
        mock_row("ghi9012", "third", "Alice"),
    ]);

    graph.search.input = "alice".to_string();
    graph.update_search_matches();

    assert_eq!(graph.search.matches, vec![0, 2]);
}

#[test]
fn test_search_matches_short_id() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        mock_row("abc1234", "first", "Alice"),
        mock_row("def5678", "second", "Bob"),
    ]);

    graph.search.input = "def".to_string();
    graph.update_search_matches();

    assert_eq!(graph.search.matches, vec![1]);
}

#[test]
fn test_search_case_insensitive() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![mock_row("abc1234", "Fix Bug", "Alice")]);

    graph.search.input = "fix bug".to_string();
    graph.update_search_matches();

    assert_eq!(graph.search.matches, vec![0]);
}

#[test]
fn test_search_next_wraps_around() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        mock_row("a", "match", "X"),
        mock_row("b", "no", "Y"),
        mock_row("c", "match", "Z"),
    ]);

    graph.search.input = "match".to_string();
    graph.update_search_matches();

    // matches = [0, 2]
    assert_eq!(graph.search.current_match, Some(0));

    graph.search_next();
    assert_eq!(graph.search.current_match, Some(1));
    assert_eq!(graph.state.selected(), Some(2)); // row index 2

    graph.search_next();
    assert_eq!(graph.search.current_match, Some(0)); // wraps
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn test_search_prev_wraps_around() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        mock_row("a", "match", "X"),
        mock_row("b", "no", "Y"),
        mock_row("c", "match", "Z"),
    ]);

    graph.search.input = "match".to_string();
    graph.update_search_matches();

    // Start at match 0
    graph.search_prev();
    assert_eq!(graph.search.current_match, Some(1)); // wraps to last
    assert_eq!(graph.state.selected(), Some(2));
}

#[test]
fn test_search_empty_input_no_matches() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![mock_row("a", "hello", "X")]);

    graph.search.input.clear();
    graph.update_search_matches();

    assert!(graph.search.matches.is_empty());
    assert_eq!(graph.search.current_match, None);
}

#[test]
fn test_search_no_results() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![mock_row("a", "hello", "Alice")]);

    graph.search.input = "zzzzz".to_string();
    graph.update_search_matches();

    assert!(graph.search.matches.is_empty());
    assert_eq!(graph.search.current_match, None);
}

fn make_label(name: &str) -> BranchLabel {
    BranchLabel {
        name: name.to_string(),
        is_head: false,
        is_remote: false,
        is_worktree: false,
        is_tag: false,
        is_stash: false,
    }
}

const OID_M: &str = "1111111111111111111111111111111111111111";
const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OID_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// Build a DAG-wired row. `oid_str` must be valid hex.
fn dag_row(
    oid_str: &str,
    short_id: &str,
    parent_oids: Vec<Oid>,
    col: usize,
    labels: Vec<BranchLabel>,
) -> GraphRow {
    GraphRow {
        commit_col: col,
        lanes: vec![LaneSegment::Commit],
        horizontal_spans: Vec::new(),
        oid: Oid::from_str(oid_str).unwrap(),
        short_id: short_id.to_string(),
        message: format!("msg-{short_id}"),
        author: "Author".to_string(),
        time: 0,
        labels,
        is_merge: parent_oids.len() > 1,
        parent_oids,
        diff_stat: None,
        collapsed: None,
    }
}

/// Standard topology for collapse tests:
/// Row 0: main0 (col=0, parents=[], labels=["main"])  ← main trunk
/// Row 1: tip   (col=1, parents=[mid], labels)         ← side branch tip
/// Row 2: mid   (col=1, parents=[main0])               ← side branch base
fn make_branch_rows(tip_labels: Vec<BranchLabel>) -> Vec<GraphRow> {
    let oid_m = Oid::from_str(OID_M).unwrap();
    let oid_b = Oid::from_str(OID_B).unwrap();

    vec![
        dag_row(OID_M, "m", vec![], 0, vec![make_label("main")]),
        dag_row(OID_A, "a", vec![oid_b], 1, tip_labels),
        dag_row(OID_B, "b", vec![oid_m], 1, vec![]),
    ]
}

#[test]
fn test_collapse_labeled_branch() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(make_branch_rows(vec![make_label("feature")]));
    // Select tip (row 1 in all_rows, row 1 in display)
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.contains(OID_A));
    // main0 + placeholder = 2 rows
    assert_eq!(graph.rows.len(), 2);
    let (_, count) = graph.rows[1].collapsed.as_ref().unwrap();
    assert_eq!(*count, 2);
    assert!(graph.rows[1].message.contains("feature"));
}

#[test]
fn test_collapse_unlabeled_merge_lane() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    // No labels on the side branch
    graph.set_rows(make_branch_rows(vec![]));
    graph.state.select(Some(2)); // select base row of side branch
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.contains(OID_A));
    assert_eq!(graph.rows.len(), 2);
    // Placeholder uses short OID since there's no label
    assert!(graph.rows[1].message.contains("a"));
}

#[test]
fn test_expand_collapsed_group() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(make_branch_rows(vec![make_label("feature")]));
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();
    assert_eq!(graph.rows.len(), 2);

    // Select the placeholder and toggle to expand
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.is_empty());
    assert_eq!(graph.display_rows().len(), 3);
}

#[test]
fn test_collapse_from_middle_of_branch() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(make_branch_rows(vec![make_label("feature")]));
    // Select the base row (row 2) — should collapse the whole segment
    graph.state.select(Some(2));
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.contains(OID_A));
    assert_eq!(graph.rows.len(), 2);
    assert!(graph.rows[1].collapsed.is_some());
}

#[test]
fn test_expand_all() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(make_branch_rows(vec![make_label("feat-a")]));
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();
    assert!(!graph.collapsed_branches.is_empty());

    graph.expand_all_branches();
    assert!(graph.collapsed_branches.is_empty());
    assert_eq!(graph.display_rows().len(), 3);
}

#[test]
fn test_main_trunk_not_collapsible() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(make_branch_rows(vec![]));
    // Select main trunk row (row 0)
    graph.state.select(Some(0));
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.is_empty());
    assert_eq!(graph.display_rows().len(), 3);
}

#[test]
fn test_interleaved_commits_collapse_together() {
    // Row 0: main0 (col=0, parents=[main1])
    // Row 1: tip_x (col=1, parents=[base_x]) -- branch X
    // Row 2: main1 (col=0, parents=[])        -- main trunk
    // Row 3: base_x (col=1, parents=[main0])  -- branch X (interleaved with main1)
    let oid_m0 = Oid::from_str(OID_M).unwrap();
    let oid_b = Oid::from_str(OID_B).unwrap();
    let oid_c = Oid::from_str(OID_C).unwrap();

    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![
        dag_row(OID_M, "m0", vec![oid_c], 0, vec![make_label("main")]),
        dag_row(OID_A, "a", vec![oid_b], 1, vec![]),
        dag_row(OID_C, "c", vec![], 0, vec![]),
        dag_row(OID_B, "b", vec![oid_m0], 1, vec![]),
    ]);

    // Select row 1 (tip of branch X)
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();

    assert!(graph.collapsed_branches.contains(OID_A));
    // Rows 1 and 3 (non-contiguous) should both be collapsed
    // main0 + placeholder + main1 = 3 rows
    assert_eq!(graph.rows.len(), 3);
    let (_, count) = graph.rows[1].collapsed.as_ref().unwrap();
    assert_eq!(*count, 2);
}

#[test]
fn test_unlabeled_branch_collapsible() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    // No labels on any side-branch row
    graph.set_rows(make_branch_rows(vec![]));
    graph.state.select(Some(1));
    graph.toggle_collapse_selected();

    assert!(!graph.collapsed_branches.is_empty());
    // Placeholder uses short OID as display name
    let placeholder = &graph.rows[1];
    assert!(placeholder.collapsed.is_some());
    assert!(placeholder.message.contains("a")); // short_id of tip
}

#[test]
fn test_abort_load_releases_latch() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    // Simulate a build that started but never reported back (panicked).
    graph.load_in_flight = true;
    graph.loading = true;

    graph.abort_load();

    assert!(!graph.load_in_flight, "latch must be released on abort");
    assert!(!graph.loading);
}

#[test]
fn test_abort_load_consumes_pending_reload() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.load_in_flight = true;
    graph.needs_reload = true;
    // No repo_path is set, so reload_graph is a no-op; we only assert the
    // coalesced-reload flag is drained and the latch is free for the next
    // real load (instead of staying stranded).
    graph.abort_load();

    assert!(!graph.load_in_flight);
    assert!(!graph.needs_reload, "pending reload flag must be consumed");
}

#[test]
fn test_abort_load_ignored_when_not_in_flight() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    // A build already reported GraphLoaded (latch clear) but its guard drops
    // late (e.g. a stats panic). The abort must not touch a fresh reload.
    graph.load_in_flight = false;
    graph.needs_reload = true;

    graph.abort_load();

    assert!(
        graph.needs_reload,
        "stale abort must not consume a pending reload"
    );
    assert!(
        graph.error.is_none(),
        "stale abort must not surface an error"
    );
}

#[test]
fn test_abort_load_bounds_repeated_panics() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    // Drive more aborts than the retry budget; each one re-arms the latch as
    // a fresh panicking build would.
    for _ in 0..=MAX_CONSECUTIVE_ABORTS {
        graph.load_in_flight = true;
        graph.needs_reload = true;
        graph.abort_load();
    }

    assert!(
        graph.error.is_some(),
        "must surface an error once the retry budget is exhausted"
    );
    assert!(
        !graph.needs_reload,
        "must stop replaying the coalesced reload after the budget"
    );
}

#[test]
fn test_set_rows_resets_abort_counter() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.load_in_flight = true;
    graph.abort_load();
    assert_eq!(graph.consecutive_aborts, 1);

    // A successful load clears the abort streak so future panics get the
    // full retry budget again.
    graph.set_rows(vec![mock_row("abc1234", "ok", "Alice")]);
    assert_eq!(graph.consecutive_aborts, 0);
}

#[test]
fn clicking_a_commit_row_opens_its_files() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.graph_list_area = Rect::new(0, 0, 80, 4);

    // A single left click (not a second one) must open the commit's files.
    let action = graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
    assert!(matches!(action, Some(Action::ShowCommitFiles { .. })));
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn highlighted_file_diff_is_requested_after_files_load() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.state.select(Some(0));

    // CommitFilesLoaded path: files arrive, first file is auto-highlighted.
    graph.set_commit_files(
        "abc1234".into(),
        "first".into(),
        vec![
            ("M".into(), "src/main.rs".into()),
            ("A".into(), "src/lib.rs".into()),
        ],
    );

    // The App then asks for the highlighted file's diff automatically; the
    // first file is selected, so the diff must target it.
    let action = graph.try_show_commit_diff().expect("auto diff");
    match action {
        Action::ShowCommitDiff { oid, file_path, .. } => {
            assert_eq!(oid, "abc1234");
            assert_eq!(file_path, std::path::PathBuf::from("src/main.rs"));
        }
        other => panic!("expected ShowCommitDiff, got {other:?}"),
    }
}

#[test]
fn moving_through_files_requests_their_diff() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_commit_files(
        "abc1234".into(),
        "first".into(),
        vec![
            ("M".into(), "a.rs".into()),
            ("M".into(), "b.rs".into()),
        ],
    );
    // Detail open, file 0 selected; pressing Down moves to file 1 and must
    // request its diff (no Enter needed).
    let action = graph
        .handle_key_event(KeyEvent::from(KeyCode::Down))
        .unwrap();
    match action {
        Some(Action::ShowCommitDiff { file_path, .. }) => {
            assert_eq!(file_path, std::path::PathBuf::from("b.rs"));
        }
        other => panic!("expected ShowCommitDiff, got {other:?}"),
    }
}

#[test]
fn clicking_a_file_row_refreshes_its_diff() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_commit_files(
        "abc1234".into(),
        "first".into(),
        vec![
            ("M".into(), "a.rs".into()),
            ("M".into(), "b.rs".into()),
        ],
    );
    if let Some(detail) = graph.commit_detail.as_mut() {
        detail.file_list_area = Rect::new(0, 0, 40, 10);
    }

    // Click the second file row (content_y = area.y + 1 = 1 → row 2 is idx 1).
    let action = graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
    match action {
        Some(Action::ShowCommitDiff { file_path, .. }) => {
            assert_eq!(file_path, std::path::PathBuf::from("b.rs"));
        }
        other => panic!("expected ShowCommitDiff, got {other:?}"),
    }
    // The highlight moved to the clicked file too.
    assert_eq!(
        graph.commit_detail.as_ref().unwrap().file_state.selected(),
        Some(1)
    );
}
