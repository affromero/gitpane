use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn submodule(name: &str) -> SubmoduleInfo {
    SubmoduleInfo {
        name: name.into(),
        path: name.into(),
        state: None,
        head: None,
        head_oid: None,
        workdir_oid: None,
        warn: SubmoduleWarn::default(),
    }
}

fn press_m(list: &mut RepoList) -> Option<Action> {
    list.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))
        .unwrap()
}

fn text_rows(list: &mut RepoList) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| list.draw(frame, frame.area()).unwrap())
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..12)
        .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

#[test]
fn repo_and_worktree_submodules_collapse_independently_and_restore_selection() {
    let mut list = make_list(&["/repo"]);
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("repo-child"));
    let mut wt = worktree_entry("feature");
    wt.checkout_submodules.push(submodule("worktree-child"));
    status.worktree_info.push(wt);
    status.stashes.push(stash_entry(0));
    list.update_status(0, status.clone());
    let rows = text_rows(&mut list);
    assert!(rows[1].contains("▾◈1"));
    assert!(rows[3].contains("▾◈1"));

    list.state.select(Some(1));
    assert!(
        matches!(press_m(&mut list), Some(Action::SelectRepo(id)) if id.0 == Path::new("/repo"))
    );
    let rows = text_rows(&mut list);
    assert!(rows[1].contains("▸◈1"));
    assert!(!rows.iter().any(|row| row.contains("repo-child")));
    assert!(rows.iter().any(|row| row.contains("worktree-child")));
    list.update_status(0, status);
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("repo-child"))
    );

    list.state.select(Some(2));
    assert!(
        matches!(press_m(&mut list), Some(Action::SelectWorktree { worktree_path, .. }) if worktree_path == Path::new("/wt/feature"))
    );
    let rows = text_rows(&mut list);
    assert!(!rows.iter().any(|row| row.contains("worktree-child")));
    assert!(rows.iter().any(|row| row.contains("▸◈1")));
    press_m(&mut list);
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("worktree-child"))
    );
    list.state.select(Some(0));
    press_m(&mut list);
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("repo-child"))
    );
}

#[test]
fn sorting_interleaves_pinned_and_status_children_and_keeps_selection() {
    let mut list = make_list(&["/repo", "/repo/beta", "/wt/feature/beta"]);
    list.repos[1].gitlink = true;
    list.repos[2].gitlink = true;
    let children = || ["zulu", "beta", "alpha"].map(submodule).to_vec();
    let mut status = empty_status("main");
    status.checkout_submodules = children();
    let mut wt = worktree_entry("feature");
    wt.checkout_submodules = children();
    status.worktree_info.push(wt);
    list.update_status(0, status);
    let check = |list: &mut RepoList, names: [&str; 3]| {
        let rows = text_rows(list);
        for start in [2, 6] {
            for (row, name) in rows[start..start + 3].iter().zip(names) {
                assert!(row.contains(&format!("{name} [sub]")), "{row}");
            }
        }
    };
    check(&mut list, ["alpha", "beta", "zulu"]);
    list.state.select(Some(1));
    let selected = list.selected_row_id();
    assert_eq!(
        selected,
        Some(RowId::Submodule("/repo".into(), "/repo/alpha".into()))
    );
    list.reverse_submodules = true;
    list.resync_rows(selected.clone());
    check(&mut list, ["zulu", "beta", "alpha"]);
    assert_eq!(list.selected_row_id(), selected);
}

#[test]
fn pinned_and_status_submodules_share_a_clickable_disclosure() {
    let mut list = make_list(&["/repo", "/repo/child", "/repo/other"]);
    list.repos[1].gitlink = true;
    list.repos[2].gitlink = true;
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("child"));
    list.update_status(0, status);
    let rows = text_rows(&mut list);
    let before = rows[1].split_once("▾◈2").unwrap().0.chars().count() as u16;
    list.state.select(Some(1));
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: before,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    assert!(
        matches!(list.handle_mouse_event(click).unwrap(), Some(Action::SelectRepo(id)) if id.0 == Path::new("/repo"))
    );
    let rows = text_rows(&mut list);
    assert!(rows[1].contains("▸◈2"));
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("child [sub]") || row.contains("other [sub]"))
    );
    list.handle_mouse_event(click).unwrap();
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
}

#[test]
fn worktree_disclosure_click_only_hides_its_own_submodules() {
    let mut list = make_list(&["/repo"]);
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("repo-child"));
    let mut wt = worktree_entry("feature");
    wt.checkout_submodules.push(submodule("worktree-child"));
    status.worktree_info.push(wt);
    list.update_status(0, status);
    let rows = text_rows(&mut list);
    let column = rows[3].split_once("▾◈1").unwrap().0.chars().count() as u16;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: 3,
        modifiers: KeyModifiers::NONE,
    };
    list.handle_mouse_event(click).unwrap();
    let rows = text_rows(&mut list);
    assert!(rows.iter().any(|row| row.contains("repo-child")));
    assert!(!rows.iter().any(|row| row.contains("worktree-child")));
    assert!(rows[3].contains("▸◈1"));
}

#[test]
fn pinned_submodules_remain_collapsed_after_repo_reordering() {
    let mut list = make_list(&["/repo", "/repo/child", "/other"]);
    list.repos[1].gitlink = true;
    list.rebuild_display_rows();
    assert!(text_rows(&mut list)[1].contains("▾◈1"));
    list.state.select(Some(1));
    assert!(
        matches!(press_m(&mut list), Some(Action::SelectRepo(id)) if id.0 == Path::new("/repo"))
    );
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
    let selected = list.selected_row_id();
    list.repos.reverse();
    list.resync_rows(selected);
    assert!(
        matches!(list.selected_row_id(), Some(RowId::Repo(path)) if path == Path::new("/repo"))
    );
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
    press_m(&mut list);
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
}

#[test]
fn clicking_another_repos_disclosure_preserves_the_selected_repo() {
    let mut list = make_list(&["/first", "/second"]);
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("child"));
    list.update_status(0, status);
    list.select_repo_row(1);
    let rows = text_rows(&mut list);
    let column = rows[1].split_once("▾◈1").unwrap().0.chars().count() as u16;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };
    assert!(list.handle_mouse_event(click).unwrap().is_none());
    assert!(
        matches!(list.selected_row_id(), Some(RowId::Repo(path)) if path == Path::new("/second"))
    );
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
}

#[test]
fn disclosure_click_accounts_for_a_missing_root_warning() {
    let mut list = make_list(&["/repo"]);
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("child"));
    list.update_status(0, status);
    list.missing_roots.push("/missing".into());
    let rows = text_rows(&mut list);
    let column = rows[2].split_once("▾◈1").unwrap().0.chars().count() as u16;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    list.handle_mouse_event(click).unwrap();
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
}

#[test]
fn temporary_worktree_absence_preserves_state_but_repo_removal_clears_it() {
    let mut list = make_list(&["/repo"]);
    let mut status = empty_status("main");
    status.checkout_submodules.push(submodule("child"));
    let mut wt = worktree_entry("feature");
    wt.checkout_submodules.push(submodule("worktree-child"));
    status.worktree_info.push(wt);
    list.update_status(0, status.clone());
    press_m(&mut list);
    list.state.select(Some(1));
    press_m(&mut list);
    let mut without_worktree = status.clone();
    without_worktree.worktree_info.clear();
    list.update_status(0, without_worktree);
    list.update_status(0, status.clone());
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("worktree-child"))
    );
    list.sync_paths(vec![]);
    list.sync_paths(vec![PathBuf::from("/repo")]);
    list.update_status(0, status);
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("worktree-child"))
    );
}

#[test]
fn clipped_submodule_disclosure_does_not_turn_the_name_into_a_toggle() {
    let mut list = make_list(&["/repo", "/repo/long-child", "/repo/long-child/grandchild"]);
    list.repos[1].gitlink = true;
    list.repos[2].gitlink = true;
    list.repos[1].status = Some(empty_status("feature/long-branch"));
    list.rebuild_display_rows();
    let (width, column) = (8..40)
        .find_map(|width| {
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            terminal
                .draw(|frame| list.draw(frame, frame.area()).unwrap())
                .unwrap();
            let layout = row_layout(
                &list.repos,
                &[],
                width - 2,
                &hierarchy::child_counts(&list.repos),
            );
            let column = 1 + layout.attention_x();
            (column < width - 1 && terminal.backend().buffer()[(column, 2)].symbol() != "▾")
                .then_some((width, column))
        })
        .expect("a clipped nested row must expose the old hit-test position");
    assert!(column < width - 1);
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    list.handle_mouse_event(click).unwrap();
    assert!(
        text_rows(&mut list)
            .iter()
            .any(|row| row.contains("grandchild [sub]"))
    );
}

#[test]
fn clicking_disclosure_after_wide_characters_uses_terminal_columns() {
    let mut list = make_list(&["/界repo"]);
    let mut status = empty_status("feature/界");
    status.checkout_submodules.push(submodule("child"));
    list.update_status(0, status);
    let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
    terminal
        .draw(|frame| list.draw(frame, frame.area()).unwrap())
        .unwrap();
    let column = (0..80)
        .find(|&x| terminal.backend().buffer()[(x, 1)].symbol() == "▾")
        .expect("visible disclosure");
    list.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row: 1,
        modifiers: KeyModifiers::NONE,
    })
    .unwrap();
    assert!(
        !text_rows(&mut list)
            .iter()
            .any(|row| row.contains("child [sub]"))
    );
}
