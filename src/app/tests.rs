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

/// Drain every action queued so far. Events translate to actions
/// synchronously, so this is the full observable outcome of `handle_event`.
fn drain_actions(app: &mut App) -> Vec<Action> {
    let mut actions = Vec::new();
    while let Ok(a) = app.action_rx.try_recv() {
        actions.push(a);
    }
    actions
}

fn power_test_app() -> App {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 2,
        ..Config::default()
    };
    App::new(config)
}

#[test]
fn deep_sleep_drops_watcher_refreshes_and_wake_repaints() {
    let mut app = power_test_app();
    drain_actions(&mut app);

    app.handle_event(Event::Power(PowerState::DeepSleep))
        .unwrap();
    app.handle_event(Event::RepoChanged("/some/repo".into()))
        .unwrap();
    assert!(
        drain_actions(&mut app).is_empty(),
        "a hidden pane must not refresh on watcher events"
    );

    app.handle_event(Event::Power(PowerState::Awake)).unwrap();
    assert!(
        drain_actions(&mut app)
            .iter()
            .any(|a| matches!(a, Action::Render)),
        "waking must repaint the stale frame"
    );
}

#[test]
fn doze_still_refreshes_on_real_repo_changes() {
    let mut app = power_test_app();
    drain_actions(&mut app);

    app.handle_event(Event::Power(PowerState::Doze)).unwrap();
    app.handle_event(Event::RepoChanged("/some/repo".into()))
        .unwrap();
    assert!(
        drain_actions(&mut app)
            .iter()
            .any(|a| matches!(a, Action::RefreshRepo(_))),
        "a visible (dozing) pane must keep tracking real changes"
    );
}

#[test]
fn root_change_while_deep_asleep_is_replayed_once_on_wake() {
    let mut app = power_test_app();
    drain_actions(&mut app);

    app.handle_event(Event::Power(PowerState::DeepSleep))
        .unwrap();
    app.handle_event(Event::ReposRootChanged).unwrap();
    app.handle_event(Event::ReposRootChanged).unwrap();
    assert!(
        drain_actions(&mut app).is_empty(),
        "no discovery walk while nobody can see the pane"
    );

    app.handle_event(Event::Power(PowerState::Awake)).unwrap();
    let discoveries = drain_actions(&mut app)
        .iter()
        .filter(|a| matches!(a, Action::DiscoverNewRepos))
        .count();
    assert_eq!(discoveries, 1, "deferred root changes coalesce into one");
}

#[test]
fn doze_to_awake_does_not_replay_or_repaint() {
    let mut app = power_test_app();
    drain_actions(&mut app);

    // Doze never deferred anything, so resuming from it is a no-op.
    app.handle_event(Event::Power(PowerState::Doze)).unwrap();
    app.handle_event(Event::Power(PowerState::Awake)).unwrap();
    assert!(drain_actions(&mut app).is_empty());
}

#[tokio::test]
async fn quit_waits_for_mutating_git_ops_and_force_quits_on_second_request() {
    let mut app = power_test_app();
    assert!(!app.ready_to_exit());

    // Quit with nothing in flight exits immediately.
    app.should_quit = true;
    assert!(app.ready_to_exit());

    // An in-flight mutating op (live GitOpGuard) defers the exit...
    let (tx, _rx) = mpsc::unbounded_channel();
    let guard = GitOpGuard::new(RepoId(std::path::PathBuf::from("/tmp/repo")), tx);
    assert!(!app.ready_to_exit());

    // ...unless the user insists with a second quit request.
    app.force_quit = true;
    assert!(app.ready_to_exit());

    // Completion of the op re-enables the normal exit path.
    app.force_quit = false;
    guard.complete();
    assert!(app.ready_to_exit());
}

/// The live-worktree refresh must drop the worktree's cached graph even
/// when the reload is deferred (commit detail open): a cache hit there would
/// resurrect stale rows for up to a poll interval. Guards the
/// `invalidate_repo(&aw.path)` line in `refresh_active_worktree`.
#[test]
fn worktree_refresh_invalidates_the_cached_graph_for_the_worktree_path() {
    let tmp = tempfile::tempdir().unwrap();
    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        ..Config::default()
    };
    let mut app = App::new(config);
    // `spawn_blocking` in the miss path needs a runtime context.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();

    let wt = tmp.path().join("live-wt");
    app.active_worktree = Some(ActiveWorktree {
        path: wt.clone(),
        repo_id: RepoId(wt.clone()),
        display_name: "live-wt".to_string(),
    });

    // Seed the cache the way a completed build would.
    app.git_graph.load_repo(wt.clone(), "live-wt");
    app.git_graph.set_rows(vec![mock_graph_row()]);
    assert!(
        app.git_graph.has_cached_graph_for(&wt),
        "precondition: the worktree graph is cached",
    );

    app.refresh_active_worktree();

    assert!(
        !app.git_graph.has_cached_graph_for(&wt),
        "refresh_active_worktree must invalidate the worktree's cached graph",
    );
}

fn mock_graph_row() -> crate::git::graph::GraphRow {
    crate::git::graph::GraphRow {
        commit_col: 0,
        lanes: vec![crate::git::graph::LaneSegment::Commit],
        horizontal_spans: Vec::new(),
        oid: git2::Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        short_id: "abc1234".to_string(),
        message: "m".to_string(),
        author: "a".to_string(),
        time: 0,
        labels: Vec::new(),
        is_merge: false,
        parent_oids: Vec::new(),
        diff_stat: None,
        collapsed: None,
    }
}

/// Removing a pinned submodule must not pollute `excluded_repos`: the walk
/// never rediscovers a repo nested inside another listed repo, and the bare
/// name would substring-match unrelated paths forever after.
#[test]
fn removing_a_pinned_submodule_leaves_excluded_repos_untouched() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "parent");
    let sub = make_repo(
        tmp.path(),
        &Path::new("parent").join("sub").display().to_string(),
    );

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        pinned_repos: vec![sub],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    let id = RepoId(
        app.repo_list
            .repos
            .iter()
            .find(|r| r.name == "sub")
            .expect("pinned submodule listed")
            .path
            .clone(),
    );

    let excluded_before = app.config.excluded_repos.clone();
    app.handle_repo_admin(Action::RemoveRepo(id)).unwrap();

    assert!(app.repo_list.repos.iter().all(|r| r.name != "sub"));
    assert_eq!(
        app.config.excluded_repos, excluded_before,
        "pinned submodule removal must not exclude by name"
    );
}

/// A repo the root walk discovered must still be excluded on removal, or it
/// reappears on the next rescan.
#[test]
fn removing_a_discovered_repo_still_excludes_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "walker");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    let id = RepoId(app.repo_list.repos[0].path.clone());

    app.handle_repo_admin(Action::RemoveRepo(id)).unwrap();

    assert!(app.repo_list.repos.is_empty());
    assert!(app.config.excluded_repos.contains(&"walker".to_string()));
}

/// `ConfirmRemoveRepo` only asks: the repo stays listed until the dialog's
/// accept action fires.
#[test]
fn confirm_remove_repo_asks_before_removing() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "keeper");

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    let id = RepoId(app.repo_list.repos[0].path.clone());

    app.handle_repo_admin(Action::ConfirmRemoveRepo(id))
        .unwrap();

    assert_eq!(app.repo_list.repos.len(), 1);
    assert!(app.confirm_dialog.visible);
}

/// Removing the repo whose path is the panels' active context (set by "Open
/// in graph" on a submodule) must drop that context, or the panels keep
/// rendering a repo that is no longer listed.
#[test]
fn removing_the_active_context_repo_clears_active_worktree() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "parent");
    let sub = make_repo(
        tmp.path(),
        &Path::new("parent").join("sub").display().to_string(),
    );

    let config = Config {
        root_dirs: vec![tmp.path().to_path_buf()],
        pinned_repos: vec![sub],
        scan_depth: 2,
        ..Config::default()
    };
    let mut app = App::new(config);
    let entry_path = app
        .repo_list
        .repos
        .iter()
        .find(|r| r.name == "sub")
        .expect("pinned submodule listed")
        .path
        .clone();
    app.active_worktree = Some(ActiveWorktree {
        path: entry_path.clone(),
        repo_id: RepoId(entry_path.clone()),
        display_name: "parent/sub".to_string(),
    });

    app.handle_repo_admin(Action::RemoveRepo(RepoId(entry_path)))
        .unwrap();

    assert!(app.active_worktree.is_none());
}

/// Adding a repo must land the user on it: Repos panel focused and the new
/// row selectable immediately (the row model is rebuilt before SelectRepo,
/// not lazily at the next draw).
#[test]
fn add_repo_focuses_the_repo_list_on_the_new_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_repo(tmp.path(), "existing");
    let newcomer = make_repo(tmp.path(), "newcomer");

    let config = Config {
        root_dirs: vec![tmp.path().join("existing-only-root")],
        ..Config::default()
    };
    std::fs::create_dir_all(tmp.path().join("existing-only-root")).unwrap();
    let mut app = App::new(config);
    app.focus = FocusPanel::Changes;

    app.handle_repo_admin(Action::AddRepo(newcomer.clone()))
        .unwrap();

    assert_eq!(app.focus, FocusPanel::Repos);
    let canonical = newcomer.canonicalize().unwrap();
    let idx = app
        .repo_list
        .repos
        .iter()
        .position(|r| r.path == canonical)
        .expect("newcomer listed");
    app.repo_list.select_repo_row(idx);
    assert_eq!(
        app.repo_list.selected_index(),
        Some(idx),
        "display_rows stale: the new row is not selectable before a draw"
    );
}
