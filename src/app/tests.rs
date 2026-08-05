use super::*;
use crate::git::status::StashEntry;
use std::path::Path;

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

/// Create a minimal repo (a `.git` directory with a `HEAD`) at `<root>/<rel>`
/// and return its path. Matches what `scanner::is_real_git_dir` accepts.
fn make_repo(root: &std::path::Path, rel: &str) -> std::path::PathBuf {
    let repo = root.join(rel);
    let dot_git = repo.join(".git");
    std::fs::create_dir_all(&dot_git).unwrap();
    std::fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    repo
}

fn displays(app: &App) -> Vec<String> {
    app.repo_list
        .repos
        .iter()
        .map(|r| r.display.clone())
        .collect()
}

/// The list must open in the order a later rescan reproduces. Discovery
/// orders by basename while rows are labelled and re-sorted by breadcrumb
/// path, so with the two out of step the list silently reshuffled the first
/// time anything triggered a rescan.
#[test]
fn app_opens_with_the_repo_list_already_sorted() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Basename order is (bravo, zulu); breadcrumb order is the reverse.
    make_repo(tmp.path(), "alpha/zulu");
    make_repo(tmp.path(), "zeta/bravo");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 3,
        ..Config::default()
    };
    let mut app = App::new(config);

    // Breadcrumb labels carry the platform's native separator.
    let zulu = Path::new("alpha").join("zulu").display().to_string();
    let bravo = Path::new("zeta").join("bravo").display().to_string();
    let at_startup = displays(&app);
    assert_eq!(at_startup, [zulu, bravo]);

    app.sort_repos();
    assert_eq!(
        displays(&app),
        at_startup,
        "first rescan reshuffled the list"
    );
}

/// Sorting is flat: a pinned repo takes its alphabetical place like any other
/// row. Pinning controls persistence (the repo survives rescans), not
/// position — a pinned `zzz` grouped above `aaa` read as "sorting is broken
/// for manually added repos".
#[test]
fn sort_repos_intermixes_pinned_repos() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "aaa");
    let pinned = make_repo(tmp.path(), "zzz");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        pinned_repos: vec![pinned],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    assert_eq!(
        displays(&app),
        ["aaa", "zzz"],
        "pin was grouped, not sorted"
    );

    app.sort_order = SortOrder::ReverseAlphabetical;
    app.sort_repos();
    assert_eq!(displays(&app), ["zzz", "aaa"]);
}

/// The sort key lowercases, so `Presentations` files between `aaa` and `zzz`
/// instead of ASCII-sorting all uppercase names to the top; Z-A is the exact
/// reverse.
#[test]
fn sort_repos_is_case_insensitive_in_both_directions() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "zzz");
    make_repo(tmp.path(), "Presentations");
    make_repo(tmp.path(), "aaa");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    assert_eq!(displays(&app), ["aaa", "Presentations", "zzz"]);

    app.sort_order = SortOrder::ReverseAlphabetical;
    app.sort_repos();
    assert_eq!(displays(&app), ["zzz", "Presentations", "aaa"]);
}

/// `s` must round-trip through every mode and come back to where it started.
#[test]
fn sort_order_cycles_through_all_modes() {
    let mut order = SortOrder::Alphabetical;
    let mut labels = Vec::new();
    for _ in 0..3 {
        labels.push(order.label());
        order = order.next();
    }
    assert_eq!(labels, ["A-Z", "Z-A", "Dirty"]);
    assert_eq!(order, SortOrder::Alphabetical);
}

/// Regression: `s` dumped the selection onto the first row. A sort changes
/// where rows live, never which row the user is on.
#[test]
fn sort_keeps_the_selected_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "aaa");
    make_repo(tmp.path(), "zzz");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    app.repo_list.select_repo_row(1); // zzz

    app.sort_order = SortOrder::ReverseAlphabetical;
    app.sort_repos();

    let selected = app.repo_list.selected_repo().unwrap();
    assert_eq!(selected.display, "zzz");
    assert_eq!(
        app.repo_list.selected_index(),
        Some(0),
        "zzz leads the Z-A order"
    );
}
