use super::*;
use crate::config::RepoSort;

/// Mirrors the `RepoStatusUpdated` handler, which needs a live terminal.
fn land_status(app: &mut App, name: &str, dirty: bool) {
    let idx = app
        .repo_list
        .repos
        .iter()
        .position(|r| r.name == name)
        .unwrap();
    let was_dirty = app.repo_list.repos[idx]
        .status
        .as_ref()
        .is_some_and(|s| s.is_dirty);
    let mut status = test_status("main", None);
    status.is_dirty = dirty;
    app.repo_list.update_status(idx, status);
    app.resort_after_status(was_dirty, dirty);
}

/// `[ui] sort` picks the startup order. Statuses land after `App::new` sorts,
/// so a dirty-first startup only shows dirty repos on top if rows move as
/// statuses arrive, and back down once a repo is clean. Status never moves
/// rows in the alphabetical orders.
#[test]
fn configured_sort_orders_rows_as_statuses_land() {
    for (sort, startup, with_zzz_dirty) in [
        (RepoSort::Alphabetical, ["aaa", "zzz"], ["aaa", "zzz"]),
        (RepoSort::Reverse, ["zzz", "aaa"], ["zzz", "aaa"]),
        (RepoSort::Dirty, ["aaa", "zzz"], ["zzz", "aaa"]),
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        make_repo(tmp.path(), "aaa");
        make_repo(tmp.path(), "zzz");
        let mut config = Config {
            write_target_override: Some(tmp.path().join("config.toml")),
            root_dirs: vec![tmp.path().to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };
        config.ui.sort = sort;
        let mut app = App::new(config).unwrap();
        assert_eq!(displays(&app), startup, "{sort:?} at startup");

        land_status(&mut app, "aaa", false);
        land_status(&mut app, "zzz", true);
        assert_eq!(displays(&app), with_zzz_dirty, "{sort:?} with zzz dirty");

        land_status(&mut app, "zzz", false);
        assert_eq!(displays(&app), startup, "{sort:?} with zzz clean again");
    }
}
