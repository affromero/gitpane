use super::render::indicator_columns;
use super::*;
use crate::components::Component;
use crate::git::status::{RepoStatus, StashEntry, WorktreeEntry};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

fn empty_status(branch: &str) -> RepoStatus {
    RepoStatus {
        branch: branch.to_string(),
        head_oid: None,
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

fn stash_entry(index: usize) -> StashEntry {
    StashEntry {
        index,
        message: format!("WIP {index}"),
        oid: format!("{index:040x}"),
    }
}

fn worktree_entry(name: &str) -> WorktreeEntry {
    WorktreeEntry {
        name: name.to_string(),
        path: PathBuf::from(format!("/wt/{name}")),
        branch: name.to_string(),
        ahead: 0,
        behind: 0,
        is_dirty: false,
        file_count: 0,
        has_dirty_submodules: false,
        has_unpushed_submodules: false,
    }
}

fn entry_with_status(path: &str, status: RepoStatus) -> RepoEntry {
    RepoEntry {
        path: PathBuf::from(path),
        name: path.trim_start_matches('/').to_string(),
        status: Some(status),
        git_op: false,
    }
}

fn make_list(paths: &[&str]) -> RepoList {
    let theme = Arc::new(Theme::default());
    RepoList::new(paths.iter().map(PathBuf::from).collect(), theme)
}

#[test]
fn sync_paths_noop_when_set_unchanged() {
    let mut list = make_list(&["/a", "/b"]);
    let diff = list.sync_paths(vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(diff.is_empty());
    assert_eq!(list.repos.len(), 2);
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
    list.expanded_repos.insert(RepoId(PathBuf::from("/b")));
    list.expanded_stashes.insert(RepoId(PathBuf::from("/b")));

    let diff = list.sync_paths(vec![PathBuf::from("/a")]);
    assert_eq!(diff.removed, vec![PathBuf::from("/b")]);
    assert!(diff.added.is_empty());
    assert_eq!(list.repos.len(), 1);
    assert!(!list.expanded_repos.contains(&RepoId(PathBuf::from("/b"))));
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

#[test]
fn indicator_columns_returns_none_when_neither_subtree_present() {
    let entry = entry_with_status("/r", empty_status("main"));
    let cols = indicator_columns(&entry, 0);
    assert_eq!(cols.stash, None);
    assert_eq!(cols.worktree, None);
}

#[test]
fn indicator_columns_locates_stash_then_worktree() {
    let mut status = empty_status("main");
    status.stashes.push(stash_entry(0));
    status.worktree_info.push(worktree_entry("wt"));
    let entry = entry_with_status("/r", status);

    let cols = indicator_columns(&entry, 0);
    // 2 (dirty marker) + 12 (branch pad for "main") + 1 (space) = 15 → stash start.
    let (s_start, s_end) = cols.stash.expect("stash range");
    let (w_start, w_end) = cols.worktree.expect("worktree range");
    assert_eq!(s_start, 15);
    // "▶$1 " = 4 columns.
    assert_eq!(s_end, 19);
    // Worktree starts immediately after stash.
    assert_eq!(w_start, 19);
    // "▶1 " = 3 columns.
    assert_eq!(w_end, 22);
}

#[test]
fn indicator_columns_accounts_for_ahead_behind() {
    let mut status = empty_status("main");
    status.ahead = 3;
    status.behind = 22;
    status.stashes.push(stash_entry(0));
    let entry = entry_with_status("/r", status);

    let cols = indicator_columns(&entry, 0);
    // 2 + 13 (branch) + 3 ("↑3 ") + 4 ("↓22 ") = 22 → stash start.
    let (s_start, _) = cols.stash.expect("stash range");
    assert_eq!(s_start, 22);
}

#[test]
fn indicator_columns_respects_long_branch_name() {
    let mut status = empty_status("feature/very-long-branch");
    status.worktree_info.push(worktree_entry("wt"));
    let entry = entry_with_status("/r", status);

    let cols = indicator_columns(&entry, 0);
    // 2 + 24 (branch chars) + 1 (space) = 27 → worktree start.
    let (w_start, _) = cols.worktree.expect("worktree range");
    assert_eq!(w_start, 27);
}

/// One repo with a linked worktree, expanded so the worktree row is
/// visible, with a render area set up for mouse hit-testing.
fn list_with_expanded_worktree() -> RepoList {
    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.worktree_info.push(worktree_entry("feature"));
    list.repos[0].status = Some(status);
    list.expanded_repos.insert(RepoId(PathBuf::from("/r")));
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
