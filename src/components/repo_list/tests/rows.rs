use super::*;

#[test]
fn selected_sync_target_id_resolves_repo_worktree_and_stash() {
    let mut list = make_list(&["/a", "/b"]);

    // Repo row: target is the repo's own path.
    list.state.select(Some(0));
    assert_eq!(
        list.selected_sync_target_id(),
        Some(RepoId(PathBuf::from("/a")))
    );

    // Worktree row: target is the worktree's path, mirroring the right-click menu.
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(1, status);
    list.state.select(Some(2)); // display rows: [/a, /b, /b's worktree]
    assert_eq!(
        list.selected_sync_target_id(),
        Some(RepoId(PathBuf::from("/wt/one")))
    );

    // Stash row: no sync target.
    let mut stash_status = empty_status("main");
    stash_status.stashes = vec![stash_entry(0)];
    list.expanded_stashes.insert(RepoId(PathBuf::from("/a")));
    list.update_status(0, stash_status);
    let stash_row = list
        .display_rows
        .iter()
        .position(|r| matches!(r, DisplayRow::Stash(_, _)))
        .expect("stash row present");
    list.state.select(Some(stash_row));
    assert_eq!(list.selected_sync_target_id(), None);
}

#[test]
fn selected_menu_targets_the_selected_row_at_its_right_edge() {
    let mut list = make_list(&["/a", "/b"]);
    list.render_area = Rect::new(2, 4, 40, 20);

    // Repo row: not a worktree, anchored at the row's right edge.
    list.state.select(Some(0));
    assert_eq!(
        list.selected_menu(),
        Some((RepoId(PathBuf::from("/a")), false, 41, 5))
    );

    // Worktree row: targets the worktree's own path and flags is_worktree.
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(1, status);
    list.state.select(Some(2)); // display rows: [/a, /b, /b's worktree]
    assert_eq!(
        list.selected_menu(),
        Some((RepoId(PathBuf::from("/wt/one")), true, 41, 7))
    );

    // Stash row: no context menu, mirroring right-click.
    let mut stash_status = empty_status("main");
    stash_status.stashes = vec![stash_entry(0)];
    list.expanded_stashes.insert(RepoId(PathBuf::from("/a")));
    list.update_status(0, stash_status);
    let stash_row = list
        .display_rows
        .iter()
        .position(|r| matches!(r, DisplayRow::Stash(_, _)))
        .expect("stash row present");
    list.state.select(Some(stash_row));
    assert_eq!(list.selected_menu(), None);
}

#[test]
fn sync_paths_noop_when_set_unchanged() {
    let mut list = make_list(&["/a", "/b"]);
    let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(diff.is_empty());
    assert_eq!(list.repos.len(), 2);
}

/// Same set in a different order must not reorder the list: the caller only
/// re-sorts on a non-empty diff, so adopting discovery order here would
/// silently override the user's sort (pinned repos jumped to the top on
/// every watcher-driven rescan).
#[test]
fn sync_paths_ignores_discovery_order() {
    let mut list = make_list(&["/a", "/b"]);
    let diff = list.sync_paths(vec![PathBuf::from("/b"), PathBuf::from("/a")]);
    assert!(diff.is_empty());
    assert_eq!(list.repos[0].path, PathBuf::from("/a"));
    assert_eq!(list.repos[1].path, PathBuf::from("/b"));
}

#[test]
fn sync_paths_reports_added_paths() {
    let mut list = make_list(&["/a"]);
    let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(!diff.is_empty());
    assert_eq!(diff.added, vec![PathBuf::from("/b")]);
    assert!(diff.removed.is_empty());
    assert_eq!(list.repos.len(), 2);
}

#[test]
fn sync_paths_reports_removed_paths_and_prunes_expansion() {
    let mut list = make_list(&["/a", "/b"]);
    list.worktrees_toggled.insert(RepoId(PathBuf::from("/b")));
    list.expanded_stashes.insert(RepoId(PathBuf::from("/b")));

    let diff = list.sync_paths(vec![PathBuf::from("/a")]);
    assert_eq!(diff.removed, vec![PathBuf::from("/b")]);
    assert!(diff.added.is_empty());
    assert_eq!(list.repos.len(), 1);
    assert!(
        !list
            .worktrees_toggled
            .contains(&RepoId(PathBuf::from("/b")))
    );
    assert!(!list.expanded_stashes.contains(&RepoId(PathBuf::from("/b"))));
}

#[test]
fn sync_paths_preserves_existing_entry_status() {
    let mut list = make_list(&["/a"]);
    list.repos[0].git_op = true;
    let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert_eq!(diff.added, vec![PathBuf::from("/b")]);
    // Original entry kept its in-progress flag.
    let a = list
        .repos
        .iter()
        .find(|r| r.path == std::path::Path::new("/a"))
        .unwrap();
    assert!(a.git_op);
    // Newly-added entry starts clean.
    let b = list
        .repos
        .iter()
        .find(|r| r.path == std::path::Path::new("/b"))
        .unwrap();
    assert!(!b.git_op);
    assert!(b.status.is_none());
}

/// A discovery change rebuilds the backing repo vector in scanner order.
/// The selected repo must stay selected even when that rebuild moves it.
#[test]
fn sync_paths_keeps_selection_when_repo_set_changes() {
    let mut list = make_list(&["/a", "/b"]);
    list.select_repo_row(1);

    list.sync_paths(vec![
        PathBuf::from("/new"),
        PathBuf::from("/a"),
        PathBuf::from("/b"),
    ]);

    assert_eq!(list.selected_repo().unwrap().path, PathBuf::from("/b"));
}

fn list_with_expanded_worktree() -> RepoList {
    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.worktree_info.push(worktree_entry("feature"));
    list.repos[0].status = Some(status);
    list.rebuild_display_rows();
    list.render_area = Rect::new(0, 0, 40, 10);
    list
}

fn right_click(row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: 5,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    }
}

#[test]
fn resolve_target_maps_worktree_path_to_its_branch_and_parent() {
    let list = list_with_expanded_worktree();
    let target = list
        .resolve_target(&RepoId(PathBuf::from("/wt/feature")))
        .expect("worktree path resolves to a target");
    assert_eq!(target.exec_path, PathBuf::from("/wt/feature"));
    assert_eq!(target.branch, "feature");
    assert_eq!(target.parent_index, 0);
    // Submodule menu items stay hidden for worktree targets.
    assert!(!target.has_submodules);
}

#[test]
fn resolve_target_maps_repo_path_to_itself() {
    let list = list_with_expanded_worktree();
    let target = list
        .resolve_target(&RepoId(PathBuf::from("/r")))
        .expect("repo path resolves to a target");
    assert_eq!(target.exec_path, PathBuf::from("/r"));
    assert_eq!(target.branch, "main");
    assert_eq!(target.parent_index, 0);
}

#[test]
fn resolve_target_returns_none_for_unknown_path() {
    let list = list_with_expanded_worktree();
    assert!(
        list.resolve_target(&RepoId(PathBuf::from("/nope")))
            .is_none()
    );
}

#[test]
fn right_click_on_worktree_row_opens_menu_targeting_the_worktree() {
    let mut list = list_with_expanded_worktree();
    // content_y = render_area.y + 1 = 1; display rows: 0 = repo, 1 = worktree,
    // so the worktree row is at mouse.row 2.
    let action = list
        .handle_mouse_event(right_click(2))
        .expect("handler ok")
        .expect("worktree right-click yields an action");
    match action {
        Action::ShowContextMenu { id, .. } => {
            assert_eq!(id, RepoId(PathBuf::from("/wt/feature")));
        }
        other => panic!("expected ShowContextMenu, got {other:?}"),
    }
}

#[test]
fn right_click_on_repo_row_opens_menu_targeting_the_repo() {
    let mut list = list_with_expanded_worktree();
    let action = list
        .handle_mouse_event(right_click(1))
        .expect("handler ok")
        .expect("repo right-click yields an action");
    match action {
        Action::ShowContextMenu { id, .. } => {
            assert_eq!(id, RepoId(PathBuf::from("/r")));
        }
        other => panic!("expected ShowContextMenu, got {other:?}"),
    }
}

#[test]
fn right_click_on_stash_row_opens_no_menu() {
    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.stashes.push(stash_entry(0));
    list.repos[0].status = Some(status);
    list.expanded_stashes.insert(RepoId(PathBuf::from("/r")));
    list.rebuild_display_rows();
    list.render_area = Rect::new(0, 0, 40, 10);
    // display rows: 0 = repo, 1 = stash → stash row is mouse.row 2.
    let action = list.handle_mouse_event(right_click(2)).expect("handler ok");
    assert!(action.is_none(), "stash rows have no context menu");
}

#[test]
fn selected_worktree_reports_worktree_when_worktree_row_is_selected() {
    let mut list = list_with_expanded_worktree();
    // display rows: 0 = repo, 1 = worktree.
    list.state.select(Some(1));
    let (repo_id, wt) = list.selected_worktree().expect("worktree selected");
    assert_eq!(repo_id, RepoId(PathBuf::from("/r")));
    assert_eq!(wt.path, PathBuf::from("/wt/feature"));
    assert_eq!(wt.branch, "feature");

    // A repo row selection is not a worktree.
    list.state.select(Some(0));
    assert!(list.selected_worktree().is_none());
}

/// Regression: a status update that adds worktree subrows to an expanded repo
/// above the selection used to shift every display index below it, silently
/// moving the selection onto a different row.
#[test]
fn update_status_keeps_selection_when_subrows_appear_above() {
    let mut list = make_list(&["/a", "/b"]);
    list.select_repo_row(1);
    assert_eq!(list.selected_repo().unwrap().path, PathBuf::from("/b"));

    // `/a`'s status arrives with two worktrees: two subrows appear above `/b`.
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one"), worktree_entry("two")];
    list.update_status(0, status);

    assert_eq!(list.selected_repo().unwrap().path, PathBuf::from("/b"));
}

/// Regression: sorting reset the selection to the first row. `resync_rows`
/// re-anchors the captured selection by repo path after any reorder.
#[test]
fn resync_rows_follows_the_selected_repo_across_a_reorder() {
    let mut list = make_list(&["/a", "/b", "/c"]);
    list.select_repo_row(2);

    let keep = list.selected_row_id();
    list.repos.reverse();
    list.resync_rows(keep);

    assert_eq!(list.selected_repo().unwrap().path, PathBuf::from("/c"));
    assert_eq!(list.selected_index(), Some(0), "/c moved to the top");
}

/// A selected worktree subrow survives a reorder; if the worktree later
/// disappears, the selection falls back to its parent repo row.
#[test]
fn resync_rows_restores_worktree_subrow_then_falls_back_to_parent() {
    let mut list = make_list(&["/a", "/b"]);
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(1, status);

    // Select /b's worktree row (display rows: [/a, /b, /b's worktree]).
    list.state.select(Some(2));
    assert!(list.selected_worktree().is_some());

    let keep = list.selected_row_id();
    list.repos.swap(0, 1);
    list.resync_rows(keep);
    let (parent, wt) = list.selected_worktree().expect("worktree row survives");
    assert_eq!(parent.0, PathBuf::from("/b"));
    assert_eq!(wt.name, "one");

    // The worktree vanishes on the next status: parent repo row takes over.
    list.update_status(0, empty_status("main"));
    assert!(list.selected_worktree().is_none());
    assert_eq!(list.selected_repo().unwrap().path, PathBuf::from("/b"));
}

/// Press `w` on the currently selected row.
fn press_w(list: &mut RepoList) {
    use crossterm::event::{KeyCode, KeyEvent};
    list.handle_key_event(KeyEvent::from(KeyCode::Char('w')))
        .unwrap();
}

fn worktree_rows(list: &RepoList) -> usize {
    list.display_rows
        .iter()
        .filter(|row| matches!(row, DisplayRow::Worktree(..)))
        .count()
}

/// Issue 49: worktrees are visible as soon as the status lands, without
/// anyone pressing `w` first.
#[test]
fn worktrees_show_by_default_once_status_arrives() {
    let mut list = make_list(&["/a"]);
    assert_eq!(worktree_rows(&list), 0, "no status yet, so no subrows");

    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one"), worktree_entry("two")];
    list.update_status(0, status);

    assert_eq!(worktree_rows(&list), 2);
}

/// `ui.expand_worktrees = false` keeps the old collapsed startup, and `w`
/// still toggles from there.
#[test]
fn expand_worktrees_disabled_starts_collapsed_and_still_toggles() {
    let mut list = make_list_with_expand(&["/a"], false);
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(0, status);
    assert_eq!(worktree_rows(&list), 0);

    list.select_repo_row(0);
    press_w(&mut list);
    assert_eq!(worktree_rows(&list), 1);

    press_w(&mut list);
    assert_eq!(worktree_rows(&list), 0);
}

/// Collapsing with `w` sticks; creating a worktree in that repo brings the
/// subrows back.
#[test]
fn expand_repo_reverses_a_session_collapse() {
    let mut list = make_list(&["/a"]);
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(0, status);

    list.select_repo_row(0);
    press_w(&mut list);
    assert_eq!(worktree_rows(&list), 0);

    // A status refresh must not undo the collapse.
    let mut status = empty_status("main");
    status.worktree_info = vec![worktree_entry("one")];
    list.update_status(0, status);
    assert_eq!(worktree_rows(&list), 0);

    list.expand_repo(&PathBuf::from("/a"));
    assert_eq!(worktree_rows(&list), 1);
}
