use super::*;
use crate::config::RepoSort;

fn land_status(app: &mut App, name: &str, dirty: bool) {
    let repo = app
        .repo_list
        .repos
        .iter()
        .find(|repo| repo.name == name)
        .unwrap();
    let id = RepoId(repo.path.clone());
    let mut status = test_status("main", None);
    status.is_dirty = dirty;
    app.handle_action_rest(Action::RepoStatusUpdated { id, status })
        .unwrap();
}

/// `[ui] sort` picks the startup order. Statuses land after `App::new` sorts,
/// so a dirty-first startup only shows dirty repos on top if rows move as
/// statuses arrive, and back down once a repo is clean. Status never moves
/// rows in the alphabetical orders.
#[tokio::test]
async fn configured_sort_orders_rows_as_statuses_land() {
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
        let selection = app.repo_list.selected_row_id().unwrap();

        land_status(&mut app, "aaa", false);
        land_status(&mut app, "zzz", true);
        assert_eq!(displays(&app), with_zzz_dirty, "{sort:?} with zzz dirty");
        assert_eq!(app.repo_list.selected_row_id(), Some(selection.clone()));

        land_status(&mut app, "zzz", false);
        assert_eq!(displays(&app), startup, "{sort:?} with zzz clean again");
        assert_eq!(app.repo_list.selected_row_id(), Some(selection));
    }
}
