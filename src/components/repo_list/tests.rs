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

fn make_list(paths: &[&str]) -> RepoList {
    let theme = Arc::new(Theme::default());
    RepoList::new(
        paths.iter().map(PathBuf::from).collect(),
        vec![], // no roots: display falls back to the basename
        theme,
    )
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
    let mut list = make_list(&["/r"]);
    list.repos[0].status = Some(empty_status("main"));
    let layout = row_layout(&list.repos, &[], 40);
    let cols = indicator_columns(&list.repos[0], 0, &layout);
    assert_eq!(cols.stash, None);
    assert_eq!(cols.worktree, None);
}

/// The status tail starts one gap after the branch column: marker (2) +
/// name "r" (1) + gap + branch "main" (4) + gap = column 9.
#[test]
fn indicator_columns_locates_stash_then_worktree() {
    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.stashes.push(stash_entry(0));
    status.worktree_info.push(worktree_entry("wt"));
    list.repos[0].status = Some(status);

    let layout = row_layout(&list.repos, &[], 40);
    let cols = indicator_columns(&list.repos[0], 0, &layout);
    // "▶$1" = 3 columns at the rail start.
    assert_eq!(cols.stash, Some((9, 12)));
    // "▶1" = 2 columns after the separating space.
    assert_eq!(cols.worktree, Some((13, 15)));
}

/// Toggles come first in the status tail; ahead/behind pack after them,
/// so the stash toggle stays at the rail start.
#[test]
fn indicator_columns_accounts_for_ahead_behind() {
    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.ahead = 3;
    status.behind = 22;
    status.stashes.push(stash_entry(0));
    list.repos[0].status = Some(status);

    let layout = row_layout(&list.repos, &[], 40);
    let cols = indicator_columns(&list.repos[0], 0, &layout);
    assert_eq!(cols.stash, Some((9, 12)));
}

/// Rows share the same attention rail regardless of name and branch length:
/// the name and branch columns are per-workspace maxima, so the stash toggle
/// sits at the same x on every row.
#[test]
fn rows_share_the_same_status_columns() {
    let mut list = make_list(&["/r", "/long-repo-name"]);
    let mut short = empty_status("main");
    short.stashes.push(stash_entry(0));
    let mut long = empty_status("feature/very-long-branch");
    long.stashes.push(stash_entry(0));
    list.repos[0].status = Some(short);
    list.repos[1].status = Some(long);

    let layout = row_layout(&list.repos, &[], 90);
    let starts: Vec<u16> = list
        .repos
        .iter()
        .map(|r| {
            indicator_columns(r, 0, &layout)
                .stash
                .expect("stash range")
                .0
        })
        .collect();
    assert_eq!(starts[0], starts[1], "attention rails are not aligned");
}

/// A single outlier branch name can't squeeze every repo name off the panel:
/// the branch column is capped to a third of the inner width.
#[test]
fn branch_cell_is_capped_to_a_third_of_the_panel() {
    let mut list = make_list(&["/r"]);
    list.repos[0].status = Some(empty_status(
        "codex/some-extremely-long-generated-branch-name",
    ));
    let layout = row_layout(&list.repos, &[], 30);
    assert_eq!(layout.branch_col, 10);
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

#[test]
fn display_path_uses_relative_path_under_a_root() {
    let roots = vec![PathBuf::from("/ws")];
    assert_eq!(
        display_path(&PathBuf::from("/ws/hbre/libmm"), &roots),
        "hbre/libmm"
    );
    // Outside every root → basename.
    assert_eq!(
        display_path(&PathBuf::from("/elsewhere/pinned"), &roots),
        "pinned"
    );
}

#[test]
fn display_path_keeps_basename_for_top_level_repo() {
    let roots = vec![PathBuf::from("/ws")];
    assert_eq!(
        display_path(&PathBuf::from("/ws/kernel-6.12"), &roots),
        "kernel-6.12"
    );
}

/// A configured root that is itself a repo (the `~/Code` case) strips to an
/// empty relative path, which would render as a blank, unclickable-looking
/// row. It falls back to the basename instead.
#[test]
fn display_path_names_a_root_that_is_itself_a_repo() {
    let roots = vec![PathBuf::from("/ws")];
    assert_eq!(display_path(&PathBuf::from("/ws"), &roots), "ws");
}

/// Breadcrumbs must survive a symlinked workspace root. Discovery hands the
/// list canonical repo paths while the config keeps the root as the user
/// wrote it, so the two only prefix-match once the root is canonicalized;
/// otherwise every label in the workspace degrades to a bare basename.
#[cfg(unix)]
#[test]
fn breadcrumbs_survive_a_symlinked_root() {
    let tmp = tempfile::TempDir::new().unwrap();
    let real = tmp.path().join("real");
    let repo = real.join("hbre").join("libmm");
    std::fs::create_dir_all(&repo).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let list = RepoList::new(
        vec![repo.canonicalize().unwrap()],
        vec![alias],
        Arc::new(Theme::default()),
    );
    assert_eq!(list.repos[0].display, "hbre/libmm");
}

#[test]
fn middle_ellipsize_keeps_head_and_tail() {
    assert_eq!(middle_ellipsize("hbre/camsys", 30), "hbre/camsys");
    let deep = "kernel-6.12/drivers/media/platform/horizon/camsys";
    assert_eq!(middle_ellipsize(deep, 20), "kernel-6.12/…/camsys");
    // Tight budget: prefer the disambiguating tail, then hard-truncate.
    assert_eq!(middle_ellipsize(deep, 12), "…/camsys");
    assert_eq!(middle_ellipsize(deep, 5), "kern…");
    assert_eq!(middle_ellipsize(deep, 0), "");
}

/// Headless render check: the list shows each repo's path relative to the
/// workspace root, so same-basename repos (e.g. three `camsys`) stay
/// distinguishable, and deep paths are middle-ellipsized to the panel width.
#[test]
fn renders_breadcrumb_paths_and_ellipsizes_deep_ones() {
    use ratatui::{Terminal, backend::TestBackend};

    let theme = Arc::new(Theme::default());
    let roots = vec![PathBuf::from("/ws")];
    let mut list = RepoList::new(
        vec![
            PathBuf::from("/ws/hbre/camsys"),
            PathBuf::from("/ws/kernel-6.1/drivers/media/platform/horizon/camsys"),
            PathBuf::from("/ws/kernel-6.12/drivers/media/platform/horizon/camsys"),
            PathBuf::from("/ws/build"),
        ],
        roots,
        theme,
    );
    // Narrow enough that the deep kernel paths must be ellipsized.
    list.render_area = Rect::new(0, 0, 40, 8);

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|f| {
            list.draw(f, f.area()).unwrap();
        })
        .unwrap();

    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();

    // Breadcrumb paths, not bare basenames — the three same-named repos are
    // told apart by their parent path.
    assert!(text.contains("hbre/camsys"), "got: {text}");
    assert!(text.contains("kernel-6.1/…/camsys"), "got: {text}");
    assert!(text.contains("kernel-6.12/…/camsys"), "got: {text}");
    assert!(text.contains("build"), "got: {text}");
}

/// Every row shows its branch — a hidden branch reads as missing data — but
/// the default branch renders in the dimmed color while deviations keep the
/// branch color.
#[test]
fn default_branch_shown_dimmed_deviating_branch_colored() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/a", "/b"]);
    list.repos[0].status = Some(empty_status("main"));
    list.repos[1].status = Some(empty_status("devel"));

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|f| {
            list.draw(f, f.area()).unwrap();
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    let row_fg_at = |row_y: usize, needle: &str| {
        let row: String = buf.content[row_y * 40..(row_y + 1) * 40]
            .iter()
            .map(|c| c.symbol())
            .collect();
        let at = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} not drawn in row {row_y}: {row}"));
        // These rows are pure ASCII, so the byte offset is the column.
        buf.content[row_y * 40 + at].style().fg
    };

    let t = &Theme::default().repo_list;
    assert_eq!(row_fg_at(1, "main"), Some(t.branch_default));
    assert_eq!(row_fg_at(2, "devel"), Some(t.branch));
}

/// Draw and hit-test must agree: the stash chevron is drawn at exactly the
/// column `indicator_columns` reports clickable. Both sides walk the same
/// `attention_cells` from the same rail — this pins them together (they
/// drifted by one column in 0.10.0).
#[test]
fn stash_toggle_glyph_sits_where_the_hit_test_says() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/r"]);
    let mut status = empty_status("main");
    status.stashes.push(stash_entry(0));
    list.repos[0].status = Some(status);

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|f| {
            list.draw(f, f.area()).unwrap();
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let row: String = buf.content[40..80].iter().map(|c| c.symbol()).collect();
    let drawn = row
        .chars()
        .position(|c| c == '\u{25b6}')
        .expect("chevron drawn") as u16;

    // Same anchors the mouse handler uses: content starts one column in from
    // the border, inner width excludes both borders.
    let layout = row_layout(&list.repos, &[], 38);
    let cols = indicator_columns(&list.repos[0], 1, &layout);
    assert_eq!(drawn, cols.stash.expect("stash range").0);
}

/// The attention cell packs each row's own indicators against the shared
/// rail — a row whose first indicator differs still starts at the same x,
/// with no columns reserved for indicators it doesn't have.
#[test]
fn attention_cell_packs_per_row_without_reserved_columns() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/a", "/b"]);
    let mut a = empty_status("main");
    a.stashes.push(stash_entry(0)); // "▶$1 ↑4"
    a.ahead = 4;
    let mut b = empty_status("main");
    b.behind = 1; // just "↓1"
    list.repos[0].status = Some(a);
    list.repos[1].status = Some(b);

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|f| {
            list.draw(f, f.area()).unwrap();
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    let glyph_x = |row_y: usize, glyph: char| -> usize {
        let row: String = buf.content[row_y * 40..(row_y + 1) * 40]
            .iter()
            .map(|c| c.symbol())
            .collect();
        row.chars()
            .position(|c| c == glyph)
            .unwrap_or_else(|| panic!("{glyph} not drawn in row {row_y}"))
    };

    let rail = 1 + row_layout(&list.repos, &[], 38).attention_x() as usize;
    assert_eq!(glyph_x(1, '\u{25b6}'), rail, "row a starts at the rail");
    assert_eq!(glyph_x(2, '\u{2193}'), rail, "row b starts at the rail too");
}

/// On a wide panel the attention rail hugs the widest name (one gap after
/// "long-repo-name") instead of drifting to the far edge; on a narrow panel
/// the name column gives way and the rail lands flush against the border.
/// Both rows share the rail either way.
#[test]
fn status_block_hugs_widest_name_and_clamps_to_the_edge() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/r", "/long-repo-name"]);
    let mut short = empty_status("main");
    short.stashes.push(stash_entry(0));
    let mut long = empty_status("main");
    long.stashes.push(stash_entry(0));
    list.repos[0].status = Some(short);
    list.repos[1].status = Some(long);

    let stash_cell = |list: &mut RepoList, width: u16, at: usize| -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal
            .draw(|f| {
                list.draw(f, f.area()).unwrap();
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        [1usize, 2]
            .iter()
            .map(|row_y| {
                let chars: Vec<char> = buf.content
                    [row_y * width as usize..(row_y + 1) * width as usize]
                    .iter()
                    .flat_map(|c| c.symbol().chars())
                    .collect();
                chars[at..at + 3].iter().collect()
            })
            .collect()
    };

    // Wide: marker (2) + widest name (14) + gap + branch "main" (4) + gap =
    // column 22; +1 for the panel border → 23. Far short of the right edge.
    for cell in stash_cell(&mut list, 40, 23) {
        assert_eq!(cell, "\u{25b6}$1");
    }
    // Narrow: the name column caps at 7 (inner 18 minus marker, gap, branch
    // column, and the 3-wide tail), putting the rail at 15; +1 border → 16,
    // flush against the right border at column 19.
    for cell in stash_cell(&mut list, 20, 16) {
        assert_eq!(cell, "\u{25b6}$1");
    }
}

/// A panel dragged down to a sliver must degrade (clip) rather than panic on
/// the zone arithmetic.
#[test]
fn narrow_panel_renders_without_panic() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/long-repo-name"]);
    let mut status = empty_status("feature/very-long-branch");
    status.ahead = 3;
    status.stashes.push(stash_entry(0));
    status.worktree_info.push(worktree_entry("wt"));
    list.repos[0].status = Some(status);

    for width in 3..=6u16 {
        let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
        terminal
            .draw(|f| {
                list.draw(f, f.area()).unwrap();
            })
            .unwrap();
        let inner = width.saturating_sub(2);
        let layout = row_layout(&list.repos, &[], inner);
        let _ = indicator_columns(&list.repos[0], 1, &layout);
    }
}

/// Focus mode dims by repo, not by row: the selected repo's worktree
/// subrows stay bright with it, and only the other repos dim. Selecting
/// the worktree subrow itself keeps its parent repo bright too.
#[test]
fn focus_mode_keeps_selected_repos_worktrees_bright() {
    use ratatui::style::Modifier;
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/a", "/b"]);
    let mut status = empty_status("main");
    status.worktree_info.push(worktree_entry("feature"));
    list.expanded_repos.insert(RepoId(PathBuf::from("/a")));
    list.update_status(0, status);
    list.repos[1].status = Some(empty_status("devel"));
    list.select_repo_row(0);

    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    let mut row_dimmed = |list: &mut RepoList, row_y: usize| {
        terminal
            .draw(|f| {
                list.draw(f, f.area()).unwrap();
            })
            .unwrap();
        terminal.backend().buffer().content[row_y * 40 + 2]
            .style()
            .add_modifier
            .contains(Modifier::DIM)
    };

    // Display rows: 1 = /a, 2 = /a's worktree, 3 = /b.
    assert!(!row_dimmed(&mut list, 1), "selected repo stays bright");
    assert!(!row_dimmed(&mut list, 2), "its worktree stays bright");
    assert!(row_dimmed(&mut list, 3), "the other repo dims");

    // Moving onto the worktree subrow keeps the whole repo bright.
    list.state.select(Some(1));
    assert!(!row_dimmed(&mut list, 1), "parent repo stays bright");
    assert!(!row_dimmed(&mut list, 2), "selected worktree stays bright");
    assert!(row_dimmed(&mut list, 3), "the other repo still dims");
}

/// Regression: a status update that adds worktree subrows to an expanded repo
/// above the selection used to shift every display index below it, silently
/// moving the selection onto a different row.
#[test]
fn update_status_keeps_selection_when_subrows_appear_above() {
    let mut list = make_list(&["/a", "/b"]);
    list.expanded_repos.insert(RepoId(PathBuf::from("/a")));
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
    list.expanded_repos.insert(RepoId(PathBuf::from("/b")));
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
