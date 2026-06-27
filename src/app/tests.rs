use super::*;
use crate::git::status::StashEntry;

#[test]
fn refresh_decision_runs_without_previous_refresh() {
    let now = Instant::now();
    assert_eq!(
        refresh_decision(None, now, Duration::from_millis(1000)),
        RefreshDecision::Now
    );
}

#[test]
fn refresh_decision_delays_inside_cooldown() {
    let last = Instant::now();
    let now = last + Duration::from_millis(250);
    assert_eq!(
        refresh_decision(Some(last), now, Duration::from_millis(1000)),
        RefreshDecision::Later(Duration::from_millis(750))
    );
}

#[test]
fn refresh_decision_runs_after_cooldown() {
    let last = Instant::now();
    let now = last + Duration::from_millis(1000);
    assert_eq!(
        refresh_decision(Some(last), now, Duration::from_millis(1000)),
        RefreshDecision::Now
    );
}

#[test]
fn graph_status_changed_ignores_file_only_changes() {
    let previous = test_status("main", Some("aaa"));
    let next = test_status("main", Some("aaa"));

    // Same HEAD and same rendered refs: a working-tree edit must not reload
    // the graph (the no-churn guarantee that keeps CPU down).
    assert!(!graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::All
    ));
}

#[test]
fn graph_status_changed_detects_head_changes() {
    let previous = test_status("main", Some("aaa"));
    let next = test_status("main", Some("bbb"));

    assert!(graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::All
    ));
}

#[test]
fn graph_status_changed_detects_stash_changes() {
    let previous = test_status("main", Some("aaa"));
    let mut next = test_status("main", Some("aaa"));
    next.stashes.push(StashEntry {
        index: 0,
        message: "WIP on main".to_string(),
        oid: "stash-oid".to_string(),
    });

    assert!(graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::All
    ));
}

#[test]
fn graph_status_changed_detects_commit_on_other_branch() {
    // The worktree case: the root's checked-out HEAD is unchanged, but a
    // commit landed on another local branch (its shared `refs/heads/*` tip
    // moved). The graph renders that branch, so it must reload.
    let previous = test_status("main", Some("aaa"));
    let mut next = test_status("main", Some("aaa"));
    next.refs.local = previous.refs.local ^ 0x9e37_79b9;

    assert!(graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::All
    ));
    // Local-only graphs render local branches too, so they also reload.
    assert!(graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::Local
    ));
}

#[test]
fn graph_status_changed_local_filter_ignores_remote_only_moves() {
    // A background fetch advances a remote-tracking ref. A local-only graph
    // does not draw it, so it must not reload; an all-branches graph does.
    let previous = test_status("main", Some("aaa"));
    let mut next = test_status("main", Some("aaa"));
    next.refs.remote = previous.refs.remote ^ 0x1234_5678;

    assert!(!graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::Local
    ));
    assert!(graph_status_changed(
        Some(&previous),
        &next,
        BranchFilter::All
    ));
}

fn test_status(branch: &str, head_oid: Option<&str>) -> RepoStatus {
    RepoStatus {
        branch: branch.to_string(),
        head_oid: head_oid.map(str::to_string),
        files: Vec::new(),
        ahead: 0,
        behind: 0,
        is_dirty: false,
        worktree_info: Vec::new(),
        has_submodules: false,
        submodules: Vec::new(),
        has_dirty_submodules: false,
        has_unpushed_submodules: false,
        fetch_failed: false,
        stashes: Vec::new(),
        refs: crate::git::status::RefsFingerprint::default(),
    }
}
