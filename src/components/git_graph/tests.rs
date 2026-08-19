use super::*;
use crate::components::Component;
use crate::config::BranchFilter;
use crate::git::graph::{BranchLabel, DiffStat, GraphRow, LaneSegment};
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

/// The commit id every `mock_row` carries, so a detail pane can be opened for
/// the row the tests click on.
const MOCK_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A graph showing one commit, with its detail pane already open on `files`,
/// laid out like the three-pane draw: graph | files | diff.
fn graph_with_open_commit(files: &[&str]) -> GitGraph {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.graph_list_area = Rect::new(0, 0, 40, 10);
    graph.diff_area = Rect::new(70, 0, 30, 10);
    let _ = graph.set_commit_files(
        MOCK_OID.to_string(),
        "first".to_string(),
        files
            .iter()
            .map(|f| ("M".to_string(), (*f).to_string()))
            .collect(),
    );
    if let Some(detail) = graph.commit_detail.as_mut() {
        detail.file_list_area = Rect::new(40, 0, 30, 10);
    }
    graph
}

/// The column of a file row, and of a point inside the diff, in the layout
/// `graph_with_open_commit` sets up. Row `n` of a bordered list is `n + 1`.
const FILE_COL: u16 = 42;
const DIFF_COL: u16 = 72;

fn left_click(graph: &mut GitGraph, column: u16, row: u16) -> Option<Action> {
    graph
        .handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap()
}

/// The debounce generation of a deferred diff request.
fn deferred_generation(action: Option<Action>) -> u64 {
    match action {
        Some(Action::ScheduleCommitDiff { generation }) => generation,
        other => panic!("expected a deferred diff request, got {other:?}"),
    }
}

/// The file a diff request targets.
fn requested_file(action: Option<Action>) -> String {
    match action {
        Some(Action::ShowCommitDiff { file_path, .. }) => file_path,
        other => panic!("expected ShowCommitDiff, got {other:?}"),
    }
}

fn selected_file(graph: &GitGraph) -> Option<usize> {
    graph.commit_detail.as_ref().unwrap().file_state.selected()
}

#[test]
fn clicking_a_commit_row_opens_its_files() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);
    graph.graph_list_area = Rect::new(0, 0, 80, 4);

    // A single left click (not a second one) must open the commit's files.
    let action = left_click(&mut graph, 1, 1);
    assert!(matches!(action, Some(Action::ShowCommitFiles { .. })));
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn clicking_the_open_commit_again_keeps_the_chosen_file() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);
    let _ = graph
        .handle_key_event(KeyEvent::from(KeyCode::Down))
        .unwrap();
    assert_eq!(selected_file(&graph), Some(1));

    // A terminal sends a double click as two clicks; re-opening the commit
    // would reload its files and throw the user back to the first one.
    let action = left_click(&mut graph, 1, 1);
    assert!(
        action.is_none(),
        "re-clicking the open commit must do nothing"
    );
    assert_eq!(selected_file(&graph), Some(1));
}

#[test]
fn opening_a_commit_asks_for_the_first_files_diff() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "first", "Alice")]);

    // The Diff pane fills itself in as the files land, with no extra keypress.
    let action = graph.set_commit_files(
        MOCK_OID.to_string(),
        "first".to_string(),
        vec![
            ("M".to_string(), "src/main.rs".to_string()),
            ("A".to_string(), "src/lib.rs".to_string()),
        ],
    );
    assert_eq!(requested_file(action), "src/main.rs");
}

#[test]
fn a_commit_that_changed_nothing_asks_for_no_diff() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(std::path::PathBuf::from("/repo"));
    graph.set_rows(vec![mock_row("abc1234", "empty", "Alice")]);

    let action = graph.set_commit_files(MOCK_OID.to_string(), "empty".to_string(), Vec::new());
    assert!(action.is_none());
}

#[test]
fn moving_through_files_while_a_diff_shows_keeps_moving_files() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);
    graph.set_commit_diff("diff --git a/a.rs".to_string());

    // The auto-shown diff must not capture the keys: `j` still walks files.
    let generation = deferred_generation(
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Char('j')))
            .unwrap(),
    );
    assert_eq!(selected_file(&graph), Some(1));
    assert_eq!(
        requested_file(graph.commit_diff_settled(generation)),
        "b.rs"
    );
}

#[test]
fn walking_past_a_file_drops_its_pending_diff() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs", "c.rs"]);

    let passed = deferred_generation(
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Down))
            .unwrap(),
    );
    let landed = deferred_generation(
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Down))
            .unwrap(),
    );

    // Only where the highlight came to rest is worth a diff.
    assert!(
        graph.commit_diff_settled(passed).is_none(),
        "the file the highlight passed through must not be diffed"
    );
    assert_eq!(requested_file(graph.commit_diff_settled(landed)), "c.rs");
}

#[test]
fn focusing_the_diff_scrolls_it_instead_of_moving_files() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);
    graph.set_commit_diff("line\n".repeat(50));

    // Enter hands the keyboard to the diff, and asks for it right away.
    assert_eq!(
        requested_file(
            graph
                .handle_key_event(KeyEvent::from(KeyCode::Enter))
                .unwrap()
        ),
        "a.rs"
    );

    let action = graph
        .handle_key_event(KeyEvent::from(KeyCode::Char('j')))
        .unwrap();
    assert!(action.is_none());
    assert_eq!(selected_file(&graph), Some(0), "files must stay put");
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 1);
}

#[test]
fn clicking_inside_the_diff_focuses_it() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);
    graph.set_commit_diff("line\n".repeat(50));

    assert!(left_click(&mut graph, DIFF_COL, 2).is_none());

    graph
        .handle_key_event(KeyEvent::from(KeyCode::Down))
        .unwrap();
    assert_eq!(graph.commit_detail.as_ref().unwrap().diff_scroll, 1);
    assert_eq!(selected_file(&graph), Some(0));
}

#[test]
fn esc_leaves_the_diff_before_closing_the_commit() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);
    graph.set_commit_diff("diff --git a/a.rs".to_string());
    let _ = graph
        .handle_key_event(KeyEvent::from(KeyCode::Enter))
        .unwrap();

    // First Esc only gives the keyboard back to the file list; the pane stays.
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Esc))
        .unwrap();
    assert!(graph.has_detail());
    let generation = deferred_generation(
        graph
            .handle_key_event(KeyEvent::from(KeyCode::Down))
            .unwrap(),
    );
    assert_eq!(selected_file(&graph), Some(1));
    assert_eq!(
        requested_file(graph.commit_diff_settled(generation)),
        "b.rs"
    );

    // The next Esc dismisses the commit detail itself.
    graph
        .handle_key_event(KeyEvent::from(KeyCode::Esc))
        .unwrap();
    assert!(!graph.has_detail());
}

#[test]
fn clicking_a_file_row_asks_for_its_diff() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);

    // Click the second file row (content_y = area.y + 1 = 1 → row 2 is idx 1).
    let generation = deferred_generation(left_click(&mut graph, FILE_COL, 2));
    assert_eq!(selected_file(&graph), Some(1));
    assert_eq!(
        requested_file(graph.commit_diff_settled(generation)),
        "b.rs"
    );
}

#[test]
fn scrolling_the_file_list_asks_for_the_settled_files_diff() {
    let mut graph = graph_with_open_commit(&["a.rs", "b.rs"]);

    let generation = deferred_generation(
        graph
            .handle_mouse_event(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: FILE_COL,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap(),
    );
    assert_eq!(selected_file(&graph), Some(1));
    assert_eq!(
        requested_file(graph.commit_diff_settled(generation)),
        "b.rs"
    );
}

// ---------------------------------------------------------------------------
// Graph cache: switching repos restores built rows instead of rebuilding.
// ---------------------------------------------------------------------------

fn graph_key(path: &str) -> GraphCacheKey {
    GraphCacheKey {
        path: PathBuf::from(path),
        branch_filter: BranchFilter::All,
        first_parent: false,
        show_stats: true,
        filters: GraphFilters::default(),
    }
}

fn cached_graph(rows: Vec<GraphRow>) -> CachedGraph {
    CachedGraph {
        rows,
        filter_branches: BTreeSet::new(),
        filter_authors: BTreeSet::new(),
    }
}

fn graph_with_path(path: &str) -> GitGraph {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.repo_path = Some(PathBuf::from(path));
    graph
}

#[test]
fn graph_cache_evicts_least_recently_used() {
    let mut cache = GraphCache::new(2);
    cache.insert(graph_key("/a"), cached_graph(vec![mock_row("1", "a", "A")]));
    cache.insert(graph_key("/b"), cached_graph(vec![mock_row("2", "b", "B")]));
    // Touching /a makes /b the least recently used.
    let _ = cache.get(&graph_key("/a"));
    cache.insert(graph_key("/c"), cached_graph(vec![mock_row("3", "c", "C")]));
    assert!(cache.get(&graph_key("/a")).is_some());
    assert!(cache.get(&graph_key("/b")).is_none());
    assert!(cache.get(&graph_key("/c")).is_some());
}

#[test]
fn graph_cache_invalidate_removes_all_signatures_for_path() {
    let mut cache = GraphCache::new(8);
    let mut key_b = graph_key("/a");
    key_b.first_parent = true;
    cache.insert(graph_key("/a"), cached_graph(vec![mock_row("1", "a", "A")]));
    cache.insert(key_b, cached_graph(vec![mock_row("2", "a1", "A")]));
    cache.invalidate(Path::new("/a"));
    assert!(cache.get(&graph_key("/a")).is_none());
    let mut key_b = graph_key("/a");
    key_b.first_parent = true;
    assert!(cache.get(&key_b).is_none());
    // Other paths are untouched.
    let mut cache2 = GraphCache::new(8);
    cache2.insert(graph_key("/a"), cached_graph(vec![mock_row("1", "a", "A")]));
    cache2.insert(graph_key("/b"), cached_graph(vec![mock_row("2", "b", "B")]));
    cache2.invalidate(Path::new("/a"));
    assert!(cache2.get(&graph_key("/b")).is_some());
}

#[test]
fn graph_cache_apply_stats_updates_cached_rows() {
    let mut cache = GraphCache::new(4);
    let row = mock_row("abc1234", "a", "A");
    let oid = row.oid;
    cache.insert(graph_key("/a"), cached_graph(vec![row]));
    let stat_map = std::collections::HashMap::from([(
        oid,
        DiffStat {
            additions: 1,
            deletions: 2,
        },
    )]);
    cache.apply_stats(&graph_key("/a"), &stat_map);
    let cached = cache.get(&graph_key("/a")).expect("still cached");
    let stat = cached.rows[0].diff_stat.as_ref().expect("stats folded in");
    assert_eq!(stat.additions, 1);
    assert_eq!(stat.deletions, 2);
}

#[test]
fn load_repo_restores_cached_rows_without_rebuilding() {
    let mut graph = graph_with_path("/tmp/cache-repo");
    graph.set_rows(vec![mock_row("abc1234", "cached message", "Alice")]);
    assert!(
        graph
            .graph_cache
            .get(&graph.cache_key(&PathBuf::from("/tmp/cache-repo")))
            .is_some()
    );

    // Switching to an uncached repo clears the view (miss path, no action_tx).
    graph.load_repo(PathBuf::from("/tmp/other-repo"), "other");
    assert!(graph.loading);
    assert!(graph.all_rows.is_empty());

    // Switching back hits the cache and restores rows synchronously.
    graph.load_repo(PathBuf::from("/tmp/cache-repo"), "cache-repo");
    assert!(!graph.loading);
    assert_eq!(graph.all_rows.len(), 1);
    assert_eq!(graph.all_rows[0].message, "cached message");
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn cache_hit_for_different_repo_resets_view_state() {
    let mut graph = graph_with_path("/tmp/a");
    graph.set_rows(vec![mock_row("1", "a", "X"), mock_row("2", "b", "Y")]);
    graph.state.select(Some(1));
    graph.search.input = "query".to_string();

    graph.load_repo(PathBuf::from("/tmp/b"), "b");
    graph.load_repo(PathBuf::from("/tmp/a"), "a");

    // The cache hit must behave like a normal repo switch: search and
    // selection are reset, then set_rows re-selects row 0.
    assert!(graph.search.input.is_empty());
    assert_eq!(graph.state.selected(), Some(0));
}

#[test]
fn invalidate_repo_forces_a_rebuild() {
    let mut graph = graph_with_path("/tmp/cache-repo");
    graph.set_rows(vec![mock_row("abc1234", "cached", "Alice")]);
    graph.invalidate_repo(&PathBuf::from("/tmp/cache-repo"));

    graph.load_repo(PathBuf::from("/tmp/other"), "other");
    graph.load_repo(PathBuf::from("/tmp/cache-repo"), "cache-repo");
    // The entry was dropped: the load is a miss, so rows stay cleared until
    // the (absent) background build reports back.
    assert!(graph.loading);
    assert!(graph.all_rows.is_empty());
}

#[test]
fn force_reload_repo_bypasses_the_cache() {
    let mut graph = graph_with_path("/tmp/cache-repo");
    graph.set_rows(vec![mock_row("abc1234", "cached", "Alice")]);

    graph.load_repo(PathBuf::from("/tmp/other"), "other");
    graph.force_reload_repo(PathBuf::from("/tmp/cache-repo"), "cache-repo");
    assert!(graph.loading);
    assert!(graph.all_rows.is_empty());
}

#[test]
fn cache_is_keyed_by_graph_options() {
    let mut graph = graph_with_path("/tmp/cache-repo");
    graph.set_rows(vec![mock_row("abc1234", "cached", "Alice")]);
    // A different build signature must not hit the cached snapshot.
    graph.graph_options.first_parent = true;

    graph.load_repo(PathBuf::from("/tmp/other"), "other");
    graph.load_repo(PathBuf::from("/tmp/cache-repo"), "cache-repo");
    assert!(graph.loading);
    assert!(graph.all_rows.is_empty());
}

#[test]
fn diff_stats_are_folded_into_the_cached_snapshot() {
    let mut graph = graph_with_path("/tmp/stats-repo");
    graph.set_rows(vec![mock_row("abc1234", "msg", "A")]);
    let oid = graph.all_rows[0].oid;
    graph.set_diff_stats(vec![(
        oid,
        DiffStat {
            additions: 3,
            deletions: 2,
        },
    )]);
    let cached = graph
        .graph_cache
        .get(&graph.cache_key(&PathBuf::from("/tmp/stats-repo")))
        .expect("cached");
    let stat = cached.rows[0].diff_stat.as_ref().expect("stats cached");
    assert_eq!(stat.additions, 3);
    assert_eq!(stat.deletions, 2);
}

#[test]
fn cache_hit_restores_filter_values_not_present_in_rows() {
    let mut graph = graph_with_path("/tmp/cache-repo");
    // A build's branch_names can list a branch whose commits are filtered out
    // of the walk; the picker still needs that value after a cache hit.
    graph.filter_branches.insert("hidden-branch".to_string());
    graph.set_rows(vec![mock_row("abc1234", "cached", "Alice")]);

    graph.load_repo(PathBuf::from("/tmp/other"), "other");
    graph.load_repo(PathBuf::from("/tmp/cache-repo"), "cache-repo");
    assert!(graph.filter_branches.contains("hidden-branch"));
}

#[test]
fn theme_change_drops_cached_row_bodies() {
    let mut graph = GitGraph::new(std::sync::Arc::new(crate::theme::Theme::default()));
    graph.set_rows(vec![mock_row("abc1234", "msg", "A")]);
    let key = RowRenderKey {
        oid: graph.all_rows[0].oid,
        theme_generation: graph.theme_generation,
        label_max_len: 24,
        dimmed: false,
        collapsed: false,
    };
    graph.render_cache.insert(key, vec![Span::raw("x")]);
    let generation_before = graph.theme_generation;
    graph.set_theme(std::sync::Arc::new(crate::theme::Theme::default()));
    assert!(graph.theme_generation > generation_before);
    assert!(graph.render_cache.is_empty());
}
