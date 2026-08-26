use super::*;
use git2::{Repository, Signature};
use tempfile::TempDir;

fn create_commit(repo: &Repository, message: &str, parents: &[&git2::Commit]) -> Oid {
    create_commit_as(repo, message, "Test", parents)
}

fn create_commit_as(
    repo: &Repository,
    message: &str,
    author: &str,
    parents: &[&git2::Commit],
) -> Oid {
    let sig = Signature::now(author, "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, parents)
        .unwrap()
}

/// Create a commit object without updating any ref, so two divergent children
/// of the same parent can be built without first-parent/HEAD fast-forward
/// conflicts. The caller points refs at the returned oid explicitly.
fn create_commit_no_ref(repo: &Repository, message: &str, parents: &[&git2::Commit]) -> Oid {
    let sig = Signature::now("Test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(None, &sig, &sig, message, &tree, parents)
        .unwrap()
}

#[test]
fn branch_filter_walks_from_the_selected_branch() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("feature", &root, false).unwrap();
    create_commit(&repo, "main-only", &[&root]);

    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some(["feature".to_string()].into_iter().collect()),
            authors: None,
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();

    assert_eq!(
        rows.iter().map(|row| &row.message).collect::<Vec<_>>(),
        ["root"]
    );
}

#[test]
fn branch_catalog_includes_refs_outside_the_active_walk() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("feature", &root, false).unwrap();
    create_commit(&repo, "main-only", &[&root]);

    let names = GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::All).unwrap();

    assert!(names.iter().any(|name| name == "feature"));
}

#[test]
fn branch_filter_hides_unselected_labels_at_a_shared_tip() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("feature", &root, false).unwrap();

    let head_name = repo.head().unwrap().shorthand().unwrap().to_string();
    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some([head_name].into_iter().collect()),
            authors: None,
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();

    assert_eq!(rows.len(), 1);
    assert!(rows[0].labels.iter().all(|label| label.name != "feature"));

    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some(["feature".to_string()].into_iter().collect()),
            authors: None,
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();
    assert!(rows[0].labels.iter().any(|label| label.name == "feature"));
}

#[test]
fn empty_branch_selection_hides_history_even_when_tags_are_enabled() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.tag_lightweight("v1.0.0", root.as_object(), false)
        .unwrap();

    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some(Default::default()),
            authors: None,
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };

    assert!(
        GraphBuilder::new()
            .build(tmp.path(), &options)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn author_filter_only_returns_selected_authors() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit_as(&repo, "keep", "Alice", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    create_commit_as(&repo, "skip", "Bob", &[&root]);

    let options = GraphOptions {
        filters: GraphFilters {
            branches: None,
            authors: Some(["Alice".to_string()].into_iter().collect()),
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message, "keep");
}

#[test]
fn test_linear_history_single_lane() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "first", &[]);
    let c1 = repo.find_commit(oid1).unwrap();
    let _oid2 = create_commit(&repo, "second", &[&c1]);

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    assert_eq!(rows.len(), 2);
    // All commits should be in column 0
    for row in &rows {
        assert_eq!(row.commit_col, 0);
    }
}

#[test]
fn test_merge_creates_two_lanes() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "root", &[]);
    let c1 = repo.find_commit(oid1).unwrap();

    // Create two divergent commits
    let sig = Signature::now("Test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();

    let oid2 = repo
        .commit(None, &sig, &sig, "branch-a", &tree, &[&c1])
        .unwrap();
    let c2 = repo.find_commit(oid2).unwrap();

    let oid3 = repo
        .commit(None, &sig, &sig, "branch-b", &tree, &[&c1])
        .unwrap();
    let c3 = repo.find_commit(oid3).unwrap();

    // Merge: first parent is c2
    let merge_oid = repo
        .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
        .unwrap();
    repo.set_head_detached(merge_oid).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    assert!(rows.len() >= 3);
    let merge_row = &rows[0];
    assert!(!merge_row.lanes.is_empty());
}

#[test]
fn test_root_commit_closes_lane() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let _oid1 = create_commit(&repo, "only", &[]);

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].commit_col, 0);
    assert_eq!(rows[0].lanes[0], LaneSegment::Commit);
}

#[test]
fn test_multiple_branches_assign_different_columns() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "root", &[]);
    let c1 = repo.find_commit(oid1).unwrap();

    let sig = Signature::now("Test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();

    let oid2 = repo
        .commit(None, &sig, &sig, "left", &tree, &[&c1])
        .unwrap();
    let c2 = repo.find_commit(oid2).unwrap();

    let oid3 = repo
        .commit(None, &sig, &sig, "right", &tree, &[&c1])
        .unwrap();
    let c3 = repo.find_commit(oid3).unwrap();

    let merge_oid = repo
        .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
        .unwrap();
    repo.set_head_detached(merge_oid).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    // After merge, we should see a fork to a second column
    let merge_row = &rows[0];
    assert!(
        merge_row.lanes.len() >= 2,
        "Expected >= 2 lanes at merge, got {}",
        merge_row.lanes.len()
    );
}

#[test]
fn test_graph_rows_carry_labels() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "first", &[]);
    let c1 = repo.find_commit(oid1).unwrap();
    let _oid2 = create_commit(&repo, "second", &[&c1]);

    // HEAD is on the default branch — tip commit should have a label
    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    let tip = &rows[0];
    assert!(
        !tip.labels.is_empty(),
        "tip commit should have at least one branch label"
    );
}

#[test]
fn test_head_marked() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let _oid1 = create_commit(&repo, "init", &[]);

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    let head_labels: Vec<_> = rows[0].labels.iter().filter(|l| l.is_head).collect();
    assert_eq!(head_labels.len(), 1, "exactly one label should be HEAD");
}

#[test]
fn test_merge_is_merge_true() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "root", &[]);
    let c1 = repo.find_commit(oid1).unwrap();

    let sig = Signature::now("Test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();

    let oid2 = repo
        .commit(None, &sig, &sig, "branch-a", &tree, &[&c1])
        .unwrap();
    let c2 = repo.find_commit(oid2).unwrap();

    let oid3 = repo
        .commit(None, &sig, &sig, "branch-b", &tree, &[&c1])
        .unwrap();
    let c3 = repo.find_commit(oid3).unwrap();

    let merge_oid = repo
        .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
        .unwrap();
    repo.set_head_detached(merge_oid).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    assert!(rows[0].is_merge, "first row should be a merge commit");
    assert!(
        !rows[1].is_merge,
        "non-merge commit should have is_merge=false"
    );
}

#[test]
fn test_merge_left_horizontal_fill() {
    let mut builder = GraphBuilder::new();
    let oid_target = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let oid_c = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();
    let oid_commit = Oid::from_str("dddddddddddddddddddddddddddddddddddddd").unwrap();

    builder.active_lanes = vec![Some(oid_target), Some(oid_b), Some(oid_c), Some(oid_commit)];

    let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_target]);

    assert_eq!(col, 3);
    assert_eq!(lanes[0], LaneSegment::RightTee);
    assert_eq!(lanes[1], LaneSegment::CrossHorizontal);
    assert_eq!(lanes[2], LaneSegment::CrossHorizontal);
    assert_eq!(lanes[3], LaneSegment::MergeLeft);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0], (0, 3, lane_color(3)));
}

#[test]
fn test_fork_right_horizontal_fill() {
    let mut builder = GraphBuilder::new();
    let oid_commit = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_active = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let oid_parent1 = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();
    let oid_parent2 = Oid::from_str("dddddddddddddddddddddddddddddddddddddd").unwrap();

    builder.active_lanes = vec![Some(oid_commit), Some(oid_active), None];

    let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_parent1, oid_parent2]);

    assert_eq!(col, 0);
    assert_eq!(lanes[0], LaneSegment::Commit);
    assert_eq!(lanes[1], LaneSegment::CrossHorizontal);
    assert_eq!(lanes[2], LaneSegment::ForkRight);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0], (0, 2, lane_color(2)));
}

#[test]
fn test_adjacent_merge_no_intermediate() {
    let mut builder = GraphBuilder::new();
    let oid_target = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_commit = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();

    builder.active_lanes = vec![Some(oid_target), Some(oid_commit)];

    let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_target]);

    assert_eq!(col, 1);
    assert_eq!(lanes[0], LaneSegment::RightTee);
    assert_eq!(lanes[1], LaneSegment::MergeLeft);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0], (0, 1, lane_color(1)));
}

#[test]
fn test_merge_right_horizontal_fill() {
    let mut builder = GraphBuilder::new();
    let oid_commit = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let oid_target = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();

    // Commit at col 0, first parent already in col 2 → MergeRight
    builder.active_lanes = vec![Some(oid_commit), Some(oid_b), Some(oid_target)];

    let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_target]);

    assert_eq!(col, 0);
    assert_eq!(lanes[0], LaneSegment::MergeRight);
    assert_eq!(lanes[1], LaneSegment::CrossHorizontal);
    assert_eq!(lanes[2], LaneSegment::LeftTee);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0], (0, 2, lane_color(0)));
}

#[test]
fn test_first_parent_simplifies_graph() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid1 = create_commit(&repo, "root", &[]);
    let c1 = repo.find_commit(oid1).unwrap();

    let sig = Signature::now("Test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();

    let oid2 = repo
        .commit(None, &sig, &sig, "branch-a", &tree, &[&c1])
        .unwrap();
    let c2 = repo.find_commit(oid2).unwrap();

    let oid3 = repo
        .commit(None, &sig, &sig, "branch-b", &tree, &[&c1])
        .unwrap();
    let c3 = repo.find_commit(oid3).unwrap();

    let merge_oid = repo
        .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
        .unwrap();
    repo.set_head_detached(merge_oid).unwrap();

    let all_opts = GraphOptions::default();
    let rows_all = GraphBuilder::new().build(tmp.path(), &all_opts).unwrap();

    let fp_opts = GraphOptions {
        first_parent: true,
        ..Default::default()
    };
    let rows_fp = GraphBuilder::new().build(tmp.path(), &fp_opts).unwrap();

    // First-parent should have fewer rows (skips branch-b)
    assert!(
        rows_fp.len() < rows_all.len(),
        "first-parent ({}) should have fewer rows than all ({})",
        rows_fp.len(),
        rows_all.len()
    );
}

#[test]
fn test_tag_labels_appear_on_tagged_commit() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid = create_commit(&repo, "tagged commit", &[]);

    // Create a lightweight tag
    let obj = repo.find_object(oid, None).unwrap();
    repo.tag_lightweight("v1.0.0", &obj, false).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    let tagged_row = rows.iter().find(|r| r.oid == oid).unwrap();
    let tag_labels: Vec<_> = tagged_row.labels.iter().filter(|l| l.is_tag).collect();
    assert_eq!(tag_labels.len(), 1);
    assert_eq!(tag_labels[0].name, "v1.0.0");
}

#[test]
fn test_stash_label_visible_when_parent_unreachable_from_head() {
    // Regression: orphaned stashes (parent commit unreachable from any
    // branch/HEAD after the user resets or deletes the branch) still
    // need to render. The revwalk is seeded from `ref_map.keys()`,
    // which `merge_stash_labels` extends with each stash's parent oid,
    // so the parent commit row gets walked even when HEAD has moved on.
    use std::fs;
    let tmp = TempDir::new().unwrap();
    let mut repo = Repository::init(tmp.path()).unwrap();

    // Commit A — root.
    let oid_a = create_commit(&repo, "A", &[]);

    // Commit B — child of A, becomes HEAD.
    let file = tmp.path().join("b.txt");
    fs::write(&file, "b").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("b.txt")).unwrap();
    index.write().unwrap();
    let oid_b = {
        let c_a = repo.find_commit(oid_a).unwrap();
        create_commit(&repo, "B", &[&c_a])
    };

    // Stash a working-tree change while HEAD is at B → stash parent = B.
    fs::write(&file, "uncommitted").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("b.txt")).unwrap();
    index.write().unwrap();
    let stasher = Signature::now("Test", "test@test.com").unwrap();
    repo.stash_save2(&stasher, Some("orphan-stash"), None)
        .unwrap();

    // Move HEAD back to A — B is now unreachable from HEAD/branches.
    let obj_a = repo.find_object(oid_a, None).unwrap();
    repo.reset(&obj_a, git2::ResetType::Hard, None).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    // B's row must still appear, carrying the stash label.
    let b_row = rows
        .iter()
        .find(|r| r.oid == oid_b)
        .expect("orphaned stash parent commit should still be walked");
    let stash_labels: Vec<_> = b_row.labels.iter().filter(|l| l.is_stash).collect();
    assert_eq!(stash_labels.len(), 1);
    assert_eq!(stash_labels[0].name, "stash@{0}");
}

#[test]
fn test_stash_label_attaches_to_parent_commit() {
    use std::fs;
    let tmp = TempDir::new().unwrap();
    let mut repo = Repository::init(tmp.path()).unwrap();

    // Initial commit so we have a HEAD to stash from.
    let initial_oid = create_commit(&repo, "init", &[]);

    // Modify the working tree so there's something to stash.
    let file = tmp.path().join("scratch.txt");
    fs::write(&file, "uncommitted change").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("scratch.txt")).unwrap();
    index.write().unwrap();

    let stasher = Signature::now("Test", "test@test.com").unwrap();
    repo.stash_save2(&stasher, Some("first stash"), None)
        .unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    let parent_row = rows
        .iter()
        .find(|r| r.oid == initial_oid)
        .expect("initial commit row");
    let stash_labels: Vec<_> = parent_row.labels.iter().filter(|l| l.is_stash).collect();
    assert_eq!(stash_labels.len(), 1, "expected one stash label");
    assert_eq!(stash_labels[0].name, "stash@{0}");
}

#[test]
fn test_tags_sort_after_branches() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let oid = create_commit(&repo, "init", &[]);

    let obj = repo.find_object(oid, None).unwrap();
    repo.tag_lightweight("v0.1", &obj, false).unwrap();

    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

    let row = &rows[0];
    // HEAD branch label should come before tag
    assert!(row.labels.len() >= 2);
    let branch_idx = row.labels.iter().position(|l| !l.is_tag).unwrap();
    let tag_idx = row.labels.iter().position(|l| l.is_tag).unwrap();
    assert!(
        branch_idx < tag_idx,
        "branch ({branch_idx}) should sort before tag ({tag_idx})"
    );
}

#[test]
fn test_filter_none_yields_no_labels() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let _oid1 = create_commit(&repo, "init", &[]);

    let options = GraphOptions {
        branch_filter: BranchFilter::None,
        ..Default::default()
    };
    let builder = GraphBuilder::new();
    let rows = builder.build(tmp.path(), &options).unwrap();

    for row in &rows {
        assert!(
            row.labels.is_empty(),
            "filter=None should produce no labels"
        );
    }
}

// --- compute_branch_segments tests ---

fn mock_segment_row(oid_str: &str, short_id: &str, parent_oids: Vec<Oid>) -> GraphRow {
    GraphRow {
        commit_col: 0,
        lanes: vec![LaneSegment::Commit],
        horizontal_spans: Vec::new(),
        oid: Oid::from_str(oid_str).unwrap(),
        short_id: short_id.to_string(),
        message: String::new(),
        author: String::new(),
        time: 0,
        labels: Vec::new(),
        is_merge: false,
        parent_oids,
        diff_stat: None,
        collapsed: None,
    }
}

#[test]
fn test_segments_linear_history_no_segments() {
    // A -> B -> C (linear, all main trunk)
    let oid_b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
    let oid_c = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();

    let rows = vec![
        mock_segment_row(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaa",
            vec![oid_b],
        ),
        mock_segment_row(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "bbb",
            vec![oid_c],
        ),
        mock_segment_row("cccccccccccccccccccccccccccccccccccccccc", "ccc", vec![]),
    ];

    let segments = compute_branch_segments(&rows);
    assert!(
        segments.is_empty(),
        "linear history should have no segments"
    );
}

#[test]
fn test_segments_simple_branch_and_merge() {
    let oid_a = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_e = Oid::from_str("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
    let oid_f = Oid::from_str("ffffffffffffffffffffffffffffffffffffffff").unwrap();

    let rows = vec![
        mock_segment_row(
            "1111111111111111111111111111111111111111",
            "merge",
            vec![oid_a, oid_e],
        ),
        mock_segment_row(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaa",
            vec![oid_f],
        ),
        mock_segment_row(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "eee",
            vec![oid_f],
        ),
        mock_segment_row("ffffffffffffffffffffffffffffffffffffffff", "fff", vec![]),
    ];

    let segments = compute_branch_segments(&rows);
    assert_eq!(segments.len(), 1, "one side branch expected");
    assert_eq!(segments[0].row_indices, vec![2]);
    assert_eq!(segments[0].id, oid_e.to_string());
}

#[test]
fn test_segments_two_independent_branches() {
    let oid_a = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_d = Oid::from_str("dddddddddddddddddddddddddddddddddddddddd").unwrap();
    let oid_e = Oid::from_str("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
    let oid_f = Oid::from_str("ffffffffffffffffffffffffffffffffffffffff").unwrap();

    let rows = vec![
        mock_segment_row(
            "1111111111111111111111111111111111111111",
            "merge",
            vec![oid_a, oid_d, oid_e],
        ),
        mock_segment_row(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaa",
            vec![oid_f],
        ),
        mock_segment_row(
            "dddddddddddddddddddddddddddddddddddddddd",
            "ddd",
            vec![oid_f],
        ),
        mock_segment_row(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "eee",
            vec![oid_f],
        ),
        mock_segment_row("ffffffffffffffffffffffffffffffffffffffff", "fff", vec![]),
    ];

    let segments = compute_branch_segments(&rows);
    assert_eq!(segments.len(), 2, "two side branches expected");
    assert_eq!(segments[0].row_indices, vec![2]);
    assert_eq!(segments[1].row_indices, vec![3]);
}

#[test]
fn test_segments_multi_commit_branch() {
    let oid_a = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let oid_d = Oid::from_str("dddddddddddddddddddddddddddddddddddddddd").unwrap();
    let oid_e = Oid::from_str("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
    let oid_f = Oid::from_str("ffffffffffffffffffffffffffffffffffffffff").unwrap();

    let rows = vec![
        mock_segment_row(
            "1111111111111111111111111111111111111111",
            "merge",
            vec![oid_a, oid_d],
        ),
        mock_segment_row(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaa",
            vec![oid_f],
        ),
        mock_segment_row(
            "dddddddddddddddddddddddddddddddddddddddd",
            "tip",
            vec![oid_e],
        ),
        mock_segment_row(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "mid",
            vec![oid_f],
        ),
        mock_segment_row("ffffffffffffffffffffffffffffffffffffffff", "fff", vec![]),
    ];

    let segments = compute_branch_segments(&rows);
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].row_indices, vec![2, 3]);
    assert_eq!(segments[0].id, oid_d.to_string());
}

#[test]
fn merged_catalog_collapses_tracked_upstream_into_local_name() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();

    // A commit only on the remote-tracking ref (ahead of local `main`).
    let remote_oid = create_commit(&repo, "remote-new", &[&root]);
    repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();

    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();

    let names = GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::All).unwrap();
    assert!(names.iter().any(|n| n == "main"), "got: {names:?}");
    assert!(
        !names.iter().any(|n| n == "origin/main"),
        "tracked upstream must collapse into its local name; got: {names:?}"
    );
}

#[test]
fn selecting_merged_branch_walks_from_both_local_and_upstream_tips() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();

    // `main` and `origin/main` diverge off `root`: each gets its own commit.
    // Neither commit is reachable from the other tip, so each appears only if
    // its own tip is used as a walk root.
    let local_oid = create_commit_no_ref(&repo, "local-new", &[&root]);
    repo.reference("refs/heads/main", local_oid, true, "test setup")
        .unwrap();
    let remote_oid = create_commit_no_ref(&repo, "remote-new", &[&root]);
    repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();

    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();

    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some(["main".to_string()].into_iter().collect()),
            authors: None,
            refs: GraphRefFilters::default(),
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();

    let messages: Vec<&str> = rows.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages.contains(&"local-new"),
        "local tip must be a walk root (commit only on the local branch); got: {messages:?}"
    );
    assert!(
        messages.contains(&"remote-new"),
        "upstream tip must be a walk root (commit only on the remote); got: {messages:?}"
    );
}

#[test]
fn remote_filter_keeps_an_upstream_branch_distinct() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();

    let remote_oid = create_commit(&repo, "remote-new", &[&root]);
    repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();
    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();

    // In Remote-only mode the catalog must show the remote branch under its own
    // name; the merge applies only when both sides are visible (All).
    let names =
        GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::Remote).unwrap();
    assert!(
        names.iter().any(|n| n == "origin/main"),
        "remote branch must stay distinct in Remote-only mode; got: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "main"),
        "local branches are not collected in Remote-only mode; got: {names:?}"
    );
}

#[test]
fn untracked_remote_branch_stays_distinct_from_any_local() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();

    // `main` tracks `origin/main`; `origin/other` is tracked by nobody. The
    // tracked pair must collapse to one entry, the untracked remote must stay
    // distinct — and no invented collapsed entry may appear.
    let tracked_oid = create_commit_no_ref(&repo, "tracked-tip", &[&root]);
    repo.reference("refs/remotes/origin/main", tracked_oid, true, "test setup")
        .unwrap();
    let other_oid = create_commit_no_ref(&repo, "other-remote-tip", &[&root]);
    repo.reference("refs/remotes/origin/other", other_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();

    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();

    let names = GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::All).unwrap();
    assert!(
        names.iter().any(|n| n == "origin/other"),
        "untracked remote must stay distinct; got: {names:?}"
    );
    assert!(names.iter().any(|n| n == "main"), "got: {names:?}");
    assert!(
        !names.iter().any(|n| n == "origin/main"),
        "tracked upstream must collapse into main; got: {names:?}"
    );
}

#[test]
fn merged_selection_respects_disabled_remote_refs() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();

    let local_oid = create_commit_no_ref(&repo, "local-new", &[&root]);
    repo.reference("refs/heads/main", local_oid, true, "test setup")
        .unwrap();
    let remote_oid = create_commit_no_ref(&repo, "remote-new", &[&root]);
    repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();
    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();

    // Turn off remote refs: the merged selection must drop the upstream tip.
    let refs = GraphRefFilters {
        remote: false,
        ..GraphRefFilters::default()
    };
    let options = GraphOptions {
        filters: GraphFilters {
            branches: Some(["main".to_string()].into_iter().collect()),
            authors: None,
            refs,
        },
        ..GraphOptions::default()
    };
    let rows = GraphBuilder::new().build(tmp.path(), &options).unwrap();
    let messages: Vec<&str> = rows.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages.contains(&"local-new"),
        "local tip must be walked; got: {messages:?}"
    );
    assert!(
        !messages.contains(&"remote-new"),
        "remote tip must be dropped when refs.remote is off; got: {messages:?}"
    );
}

#[test]
fn ambiguous_shared_upstream_keeps_remote_distinct() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();
    repo.branch("dev", &root, false).unwrap();

    let remote_oid = create_commit(&repo, "remote-tip", &[&root]);
    repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();

    // Both `main` and `dev` track the SAME upstream `origin/main`. The collapse
    // is ambiguous, so the remote must stay its own distinct catalog entry.
    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();
    let mut dev_branch = repo.find_branch("dev", git2::BranchType::Local).unwrap();
    dev_branch.set_upstream(Some("origin/main")).unwrap();

    let names = GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::All).unwrap();
    assert!(
        names.iter().any(|n| n == "origin/main"),
        "ambiguous shared upstream must stay distinct; got: {names:?}"
    );
    assert!(names.iter().any(|n| n == "main"), "got: {names:?}");
    assert!(names.iter().any(|n| n == "dev"), "got: {names:?}");
}
