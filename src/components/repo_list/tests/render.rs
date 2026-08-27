use super::indicator_columns;
use super::*;

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
        true,
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

#[test]
fn middle_ellipsize_handles_windows_paths() {
    // A native Windows path uses backslashes. It must get the same
    // middle-ellipsis treatment as a POSIX path — keeping the repo basename
    // visible — instead of landing in one giant part and hard-truncating
    // the tail off (the regression behind the Windows CI failure).
    let deep = r"C:\Users\RUNNER~\.config\gitpane\config\gone";
    assert_eq!(middle_ellipsize(deep, 30), "C:/…/gone");
    assert_eq!(middle_ellipsize(deep, 6), "…/gone");
    assert_eq!(middle_ellipsize(deep, 5), "C:\\U…");
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
        true,
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

/// A configured root that doesn't exist on disk surfaces a persistent hint
/// above the list (discovery silently skips it), while repos that do exist
/// still render normally below it.
#[test]
fn renders_persistent_hint_for_missing_root_beside_normal_repos() {
    use ratatui::{Terminal, backend::TestBackend};

    let theme = Arc::new(Theme::default());
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().to_path_buf();
    let missing = tmp.path().join("gone"); // never created
    let mut list = RepoList::new(
        vec![existing.join("repo-a"), existing.join("repo-b")],
        vec![existing.clone(), missing.clone()],
        true,
        theme,
    );
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

    // The tmp path overflows the 40-wide panel, so the hint's path is
    // middle-ellipsized — match the parts, not the full path.
    assert!(text.contains("root does not exist:"), "got: {text}");
    assert!(text.contains("gone"), "got: {text}");
    assert!(text.contains("repo-a"), "got: {text}");
    assert!(text.contains("repo-b"), "got: {text}");
}

#[test]
fn missing_root_hint_reserves_space_for_the_label() {
    use ratatui::{Terminal, backend::TestBackend};

    let theme = Arc::new(Theme::default());
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().to_path_buf();
    // Long enough that ellipsizing lands in the hard-truncation branch:
    // the 3-cell " ! " label must be reserved from the budget, or the
    // paragraph clips the trailing ellipsis marker off the line.
    let missing = tmp
        .path()
        .join("this-repository-basename-is-very-long-indeed");
    let mut list = RepoList::new(
        vec![existing.join("repo-a")],
        vec![existing.clone(), missing.clone()],
        true,
        theme,
    );
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
    assert!(
        text.contains('…'),
        "ellipsis must survive the label budget: {text}"
    );
}

/// When every configured root exists, no hint is rendered — the hint is
/// reserved for the silent-skip case, not a permanent fixture of the panel.
#[test]
fn renders_no_hint_when_all_roots_exist() {
    use ratatui::{Terminal, backend::TestBackend};

    let theme = Arc::new(Theme::default());
    let tmp = tempfile::tempdir().unwrap();
    let mut list = RepoList::new(
        vec![tmp.path().join("repo-a")],
        vec![tmp.path().to_path_buf()],
        true,
        theme,
    );
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

    assert!(text.contains("repo-a"), "got: {text}");
    assert!(!text.contains("root does not exist"), "got: {text}");
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

/// Focus mode dims by repo, not by row: the selected repo's worktree and
/// stash subrows stay bright with it, and only the other repos dim.
/// Selecting a subrow itself keeps the whole repo bright too.
#[test]
fn focus_mode_keeps_selected_repos_subrows_bright() {
    use ratatui::style::Modifier;
    use ratatui::{Terminal, backend::TestBackend};

    let mut list = make_list(&["/a", "/b"]);
    let mut status = empty_status("main");
    status.worktree_info.push(worktree_entry("feature"));
    status.stashes.push(stash_entry(0));
    list.expanded_stashes.insert(RepoId(PathBuf::from("/a")));
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

    // Display rows: 1 = /a, 2 = /a's worktree, 3 = /a's stash, 4 = /b.
    assert!(!row_dimmed(&mut list, 1), "selected repo stays bright");
    assert!(!row_dimmed(&mut list, 2), "its worktree stays bright");
    assert!(!row_dimmed(&mut list, 3), "its stash stays bright");
    assert!(row_dimmed(&mut list, 4), "the other repo dims");

    // Moving onto a subrow keeps the whole repo bright.
    for subrow in [1, 2] {
        list.state.select(Some(subrow));
        assert!(!row_dimmed(&mut list, 1), "parent repo stays bright");
        assert!(!row_dimmed(&mut list, 2), "worktree stays bright");
        assert!(!row_dimmed(&mut list, 3), "stash stays bright");
        assert!(row_dimmed(&mut list, 4), "the other repo still dims");
    }
}
