use super::*;

#[cfg(test)]
mod tag_tests {
    use super::*;

    fn rendered(
        state: Option<SubmoduleState>,
        head: Option<SubmoduleHead>,
        warn: SubmoduleWarn,
    ) -> String {
        let theme = FileListTheme::default();
        submodule_tag_spans(&state, &head, &warn, &theme)
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn modified_clean() {
        assert_eq!(
            rendered(
                Some(SubmoduleState::Modified),
                None,
                SubmoduleWarn::default()
            ),
            "[sub: +commit] "
        );
    }

    #[test]
    fn modified_with_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 3,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), None, warn),
            "[sub: +commit \u{2191}3] "
        );
    }

    #[test]
    fn modified_with_unreachable_takes_precedence_over_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 5,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), None, warn),
            "[sub: +commit \u{26a0}unreach] "
        );
    }

    #[test]
    fn dirty_clean() {
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), None, SubmoduleWarn::default()),
            "[sub: ~dirty] "
        );
    }

    #[test]
    fn dirty_with_unpushed() {
        let warn = SubmoduleWarn {
            unpushed_commits: 1,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), None, warn),
            "[sub: ~dirty \u{2191}1] "
        );
    }

    #[test]
    fn dirty_with_unreachable() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Dirty), None, warn),
            "[sub: ~dirty \u{26a0}unreach] "
        );
    }

    #[test]
    fn uninitialized_skips_warn() {
        // Even with warn fields set, uninitialized always renders just `-uninit`.
        let warn = SubmoduleWarn {
            unpushed_commits: 7,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Uninitialized), None, warn),
            "[sub: -uninit] "
        );
    }

    #[test]
    fn unreach_only_synthetic_row() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
        assert_eq!(rendered(None, None, warn), "[sub: \u{26a0}unreach] ");
    }

    #[test]
    fn unpushed_only_synthetic_row() {
        let warn = SubmoduleWarn {
            unpushed_commits: 4,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        };
        assert_eq!(rendered(None, None, warn), "[sub: \u{2191}4] ");
    }

    #[test]
    fn no_state_no_warn_falls_back_to_plain_tag() {
        assert_eq!(
            rendered(None, None, SubmoduleWarn::default()),
            "[submodule] "
        );
    }

    #[test]
    fn modified_with_branch() {
        assert_eq!(
            rendered(
                Some(SubmoduleState::Modified),
                Some(SubmoduleHead::Branch("feature".to_string())),
                SubmoduleWarn::default(),
            ),
            "[sub: +commit @feature] "
        );
    }

    #[test]
    fn dirty_detached() {
        assert_eq!(
            rendered(
                Some(SubmoduleState::Dirty),
                Some(SubmoduleHead::Detached),
                SubmoduleWarn::default(),
            ),
            "[sub: ~dirty @detached] "
        );
    }

    #[test]
    fn modified_needs_merge_to_default() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: false,
            needs_merge_to_default: true,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), None, warn),
            "[sub: +commit \u{219b}main] "
        );
    }

    #[test]
    fn composed_branch_unpushed_and_needs_merge() {
        let warn = SubmoduleWarn {
            unpushed_commits: 3,
            pointer_unreachable: false,
            needs_merge_to_default: true,
        };
        assert_eq!(
            rendered(
                Some(SubmoduleState::Modified),
                Some(SubmoduleHead::Branch("feature".to_string())),
                warn,
            ),
            "[sub: +commit @feature \u{2191}3 \u{219b}main] "
        );
    }

    #[test]
    fn unreachable_dominates_needs_merge() {
        // On no remote at all: only `⚠unreach`, never `↛main`.
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: true,
        };
        assert_eq!(
            rendered(Some(SubmoduleState::Modified), None, warn),
            "[sub: +commit \u{26a0}unreach] "
        );
    }

    #[test]
    fn uninitialized_suppresses_branch_and_merge() {
        let warn = SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: false,
            needs_merge_to_default: true,
        };
        assert_eq!(
            rendered(
                Some(SubmoduleState::Uninitialized),
                Some(SubmoduleHead::Branch("x".to_string())),
                warn,
            ),
            "[sub: -uninit] "
        );
    }
}

#[cfg(test)]
mod selected_menu_tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(path: &str, status: FileStatus, staged: bool, unstaged: bool) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            status,
            staged,
            unstaged,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        }
    }

    #[test]
    fn selected_menu_builds_a_file_context_menu_action() {
        let mut list = FileList::new(Arc::new(Theme::default()));
        list.render_area = Rect::new(0, 0, 60, 24);
        list.set_files(
            vec![
                entry("a.rs", FileStatus::Modified, false, true),
                entry("b.rs", FileStatus::Untracked, false, true),
            ],
            "repo",
            RepoId(PathBuf::from("/repo")),
        );
        list.state.select(Some(1));
        let action = list.selected_menu().expect("menu for selected row");
        assert!(matches!(
            action,
            Action::ShowFileContextMenu {
                id,
                path,
                row: 2,
                col: 59,
                staged: false,
                unstaged: true,
                is_untracked: true,
                is_submodule: false,
            } if id.0.as_path() == std::path::Path::new("/repo")
                && path.as_path() == std::path::Path::new("b.rs")
        ));
    }

    #[test]
    fn selected_menu_none_without_files() {
        let list = FileList::new(Arc::new(Theme::default()));
        assert!(list.selected_menu().is_none());
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::git::status::{FileEntry, FileStatus, SubmoduleWarn};
    use std::path::PathBuf;

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            status: FileStatus::Modified,
            staged: false,
            unstaged: true,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        }
    }

    #[test]
    fn search_opens_match_without_navigation_and_handles_refresh() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        let repo = RepoId(PathBuf::from("/r"));
        fl.set_files(vec![entry("main.rs"), entry("lib.rs")], "r", repo.clone());
        fl.handle_key_event(KeyCode::Char('/').into()).unwrap();
        for c in "lib".chars() {
            fl.handle_key_event(KeyCode::Char(c).into()).unwrap();
        }
        assert!(
            matches!(fl.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowDiff(_, p)) if p == std::path::Path::new("lib.rs"))
        );
        fl.set_files(vec![entry("main.rs")], "r", repo);
        assert!(
            fl.handle_key_event(KeyCode::Enter.into())
                .unwrap()
                .is_none()
        );
        fl.set_files(
            vec![entry("other.rs")],
            "other",
            RepoId(PathBuf::from("/other")),
        );
        assert!(!fl.search_active());
        assert!(
            matches!(fl.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowDiff(_, p)) if p == std::path::Path::new("other.rs"))
        );
    }

    #[test]
    fn clicking_nonmatch_does_not_replace_the_search_selection() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.set_files(
            vec![entry("main.rs"), entry("lib.rs")],
            "r",
            RepoId(PathBuf::from("/r")),
        );
        fl.handle_key_event(KeyCode::Char('/').into()).unwrap();
        fl.handle_key_event(KeyCode::Char('l').into()).unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        for button in [MouseButton::Left, MouseButton::Right] {
            assert!(
                fl.handle_mouse_event(MouseEvent {
                    kind: MouseEventKind::Down(button),
                    column: 2,
                    row: 1,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                })
                .unwrap()
                .is_none()
            );
        }
        assert!(
            matches!(fl.handle_key_event(KeyCode::Enter.into()).unwrap(),
            Some(Action::ShowDiff(_, p)) if p == std::path::Path::new("lib.rs"))
        );
    }

    #[test]
    fn filter_jumps_to_matching_file_and_opens_it() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.set_files(
            vec![
                entry("src/main.rs"),
                entry("src/lib.rs"),
                entry("tests/foo.rs"),
            ],
            "r",
            RepoId(PathBuf::from("/r")),
        );
        fl.handle_key_event(KeyEvent::from(KeyCode::Char('/')))
            .unwrap();
        for c in "lib".chars() {
            fl.handle_key_event(KeyEvent::from(KeyCode::Char(c)))
                .unwrap();
        }
        assert_eq!(fl.filter.count(), 1);
        fl.select_next();
        assert_eq!(fl.state.selected(), Some(1));
        let action = fl.try_show_diff();
        assert!(
            matches!(action, Some(Action::ShowDiff(_, p)) if p == std::path::Path::new("src/lib.rs"))
        );
    }

    #[test]
    fn filter_with_no_matches_clears_selection_and_does_not_open_diff() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.set_files(vec![entry("a.txt")], "r", RepoId(PathBuf::from("/r")));
        fl.handle_key_event(KeyEvent::from(KeyCode::Char('/')))
            .unwrap();
        for c in "zzz".chars() {
            fl.handle_key_event(KeyEvent::from(KeyCode::Char(c)))
                .unwrap();
        }
        assert_eq!(fl.filter.count(), 0);
        fl.select_next();
        assert!(fl.selected_path().is_none());
        assert!(
            fl.handle_key_event(KeyCode::Enter.into())
                .unwrap()
                .is_none()
        );
        // Esc clears the search and restores full navigation
        fl.handle_key_event(KeyEvent::from(KeyCode::Esc)).unwrap();
        assert!(!fl.filter.is_active());
    }
}

#[cfg(test)]
mod diff_scroll_indicator_tests {
    use super::*;
    use crate::components::scroll_pane::{ScrollLayout, THUMB, TRACK, bordered_inner};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::path::PathBuf;

    /// A `FileList` showing a diff of `lines` rows, drawn so the diff pane's
    /// geometry is known.
    fn list_with_diff(
        lines: usize,
    ) -> (FileList, ratatui::Terminal<ratatui::backend::TestBackend>) {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.set_diff(
            (0..lines)
                .map(|i| format!("+line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        (fl, terminal)
    }

    /// Every row of the rendered frame as a string.
    fn rows(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// The scroll-indicator column inside the diff pane, top row first.
    fn indicator_column(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        pane: Rect,
    ) -> String {
        let rows = rows(terminal);
        let x = usize::from(pane.x + pane.width - 2);
        (pane.y + 1..pane.y + pane.height - 1)
            .map(|y| rows[usize::from(y)].chars().nth(x).unwrap_or(' '))
            .collect()
    }

    #[test]
    fn diff_counter_tracks_the_scroll_and_never_scrolls_past_the_end() {
        let (mut fl, mut terminal) = list_with_diff(60);
        let pane = fl.diff_area;
        let visible = pane.height - 2;
        assert!(visible > 0 && visible < 60, "pane shows {visible} of 60");

        // Top of the diff: counter counts the rows on screen, no blank space yet.
        assert!(rows(&terminal)[usize::from(pane.y)].contains(&format!("{visible}/60")));
        assert!(indicator_column(&terminal, pane).starts_with(THUMB));

        // Scrolling down far past the end parks on the last screenful: the pane
        // keeps its content instead of scrolling into empty rows.
        for _ in 0..200 {
            fl.handle_key_event(KeyEvent::from(KeyCode::Char('j')))
                .unwrap();
        }
        assert_eq!(fl.diff_scroll, 60 - visible);
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        assert!(rows(&terminal)[usize::from(pane.y)].contains("60/60"));
        assert!(indicator_column(&terminal, pane).ends_with(THUMB));

        // And back up stops at the top.
        for _ in 0..200 {
            fl.handle_key_event(KeyEvent::from(KeyCode::Char('k')))
                .unwrap();
        }
        assert_eq!(fl.diff_scroll, 0);
    }

    #[test]
    fn diff_that_fits_shows_no_indicator() {
        let (fl, terminal) = list_with_diff(3);
        let pane = fl.diff_area;
        assert!(!indicator_column(&terminal, pane).contains(THUMB));
        let title = &rows(&terminal)[usize::from(pane.y)];
        assert!(
            !title
                .split_whitespace()
                .any(|token| token
                    .split_once('/')
                    .is_some_and(|(seen, total)| !seen.is_empty()
                        && seen.bytes().all(|b| b.is_ascii_digit())
                        && !total.is_empty()
                        && total.bytes().all(|b| b.is_ascii_digit()))),
            "a diff that fits keeps a bare title: {title:?}"
        );
    }

    #[test]
    fn diff_track_is_drawn_beside_the_wrapped_content() {
        // A line far longer than the pane wraps inside the narrowed content
        // column: the wrapped text has to reach the content column that sits
        // right beside the thumb, proving the thumb claims a column of its own
        // instead of covering the text's last one.
        let mut fl = FileList::new(Arc::new(Theme::default()));
        // Six wrapped lines to overflow the pane, then a short one.
        let long = format!("+{}\n", "x".repeat(500));
        fl.set_diff(long.repeat(6) + "+short\n");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        let pane = fl.diff_area;

        let column = indicator_column(&terminal, pane);
        assert!(
            column.contains(TRACK) && column.chars().all(|c| c == '█' || c == '│'),
            "indicator column: {column:?}"
        );

        // The last content column (the cell left of the thumb) carries the
        // wrapped line's 500th character, not a trace of the indicator.
        let cell = |row: u16, x: u16| terminal.backend().buffer()[(x, row)].symbol().to_owned();
        let content_right = pane.x + pane.width - 3;
        let first_row = pane.y + 1;
        assert_eq!(
            cell(first_row, content_right),
            "x",
            "wrapped content fills the pane up to the indicator column"
        );
        let bar_cell = cell(first_row, content_right + 1);
        assert!(
            bar_cell == THUMB || bar_cell == TRACK,
            "the column beside the content belongs to the indicator: {bar_cell:?}"
        );
    }

    /// One mouse event, the way the `App` routes them to the panel under the
    /// pointer.
    fn mouse(fl: &mut FileList, kind: MouseEventKind, column: u16, row: u16) -> Option<Action> {
        fl.handle_mouse_event(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap()
    }

    /// The largest offset the diff pane as drawn allows.
    fn diff_max(fl: &FileList) -> u16 {
        ScrollLayout::for_text(
            fl.diff_content.as_deref().unwrap(),
            bordered_inner(fl.diff_area),
            0,
        )
        .gauge
        .max_offset()
    }

    #[test]
    fn clicking_and_dragging_the_diff_indicator_scrubs_the_pane() {
        let (mut fl, mut terminal) = list_with_diff(60);
        let pane = fl.diff_area;
        let bar = pane.x + pane.width - 2;
        let max = diff_max(&fl);

        // Click the middle of the track: the pane jumps to the middle.
        mouse(
            &mut fl,
            MouseEventKind::Down(MouseButton::Left),
            bar,
            pane.y + pane.height / 2,
        );
        let middle = fl.diff_scroll;
        assert!(
            middle > 0 && middle < max,
            "a click in the middle of the track must land in the middle: {middle} of {max}"
        );

        // Click the bottom, then drag up to the top: the drag keeps scrubbing.
        mouse(
            &mut fl,
            MouseEventKind::Down(MouseButton::Left),
            bar,
            pane.y + pane.height - 2,
        );
        assert_eq!(fl.diff_scroll, max);
        mouse(
            &mut fl,
            MouseEventKind::Drag(MouseButton::Left),
            bar,
            pane.y + 1,
        );
        assert_eq!(fl.diff_scroll, 0);
        // Sideways motion keeps the grab: only the row matters.
        mouse(
            &mut fl,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 2,
            pane.y + pane.height - 2,
        );
        assert_eq!(fl.diff_scroll, max);

        // Releasing ends the grab, so later motion is not a scrub.
        mouse(
            &mut fl,
            MouseEventKind::Up(MouseButton::Left),
            bar,
            pane.y + 1,
        );
        mouse(
            &mut fl,
            MouseEventKind::Drag(MouseButton::Left),
            bar,
            pane.y + 1,
        );
        assert_eq!(fl.diff_scroll, max, "a drag without a press must not scrub");

        // The frame after a scrub shows the position it selected.
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        assert!(rows(&terminal)[usize::from(pane.y)].contains("60/60"));
    }

    #[test]
    fn clicking_beside_the_diff_indicator_does_not_scrub() {
        let (mut fl, _) = list_with_diff(60);
        let pane = fl.diff_area;
        // One column left of the indicator is content, not the scrollbar.
        mouse(
            &mut fl,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + pane.width - 3,
            pane.y + pane.height - 2,
        );
        assert_eq!(fl.diff_scroll, 0);
        assert!(fl.dragging_scrollbar.is_none());
    }

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            status: FileStatus::Modified,
            staged: false,
            unstaged: true,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        }
    }

    #[test]
    fn a_file_row_click_at_the_indicator_column_still_selects_the_row() {
        // Stacked split (`horizontal_layout`): the file list and the diff share the
        // indicator's column, so only the row tells them apart.
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.horizontal_layout = true;
        fl.set_files(
            vec![entry("a.rs"), entry("b.rs"), entry("c.rs")],
            "repo",
            RepoId(PathBuf::from("/repo")),
        );
        fl.set_diff(
            (0..60)
                .map(|i| format!("+line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();

        let diff = fl.diff_area;
        let row = fl.file_list_area.y + 2; // second file row
        let column = diff.x + diff.width - 2; // the indicator's column
        assert!(
            fl.file_list_area
                .contains(ratatui::layout::Position::new(column, row)),
            "the row click has to be inside the list"
        );
        mouse(
            &mut fl,
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
        );
        assert_eq!(
            fl.state.selected(),
            Some(1),
            "a click above the diff pane must select the file row"
        );
        assert_eq!(fl.diff_scroll, 0, "and must not scrub the diff");
    }

    #[test]
    fn a_cancelled_grab_stops_scrubbing_the_diff() {
        let (mut fl, mut terminal) = list_with_diff(60);
        let pane = fl.diff_area;
        let bar = pane.x + pane.width - 2;
        mouse(
            &mut fl,
            MouseEventKind::Down(MouseButton::Left),
            bar,
            pane.y + 1,
        );
        assert!(fl.dragging_scrollbar.is_some(), "the press armed the grab");

        fl.cancel_drag();
        assert!(fl.dragging_scrollbar.is_none());
        fl.set_diff(
            (0..60)
                .map(|i| format!("+line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        terminal.draw(|f| fl.draw(f, f.area()).unwrap()).unwrap();
        mouse(
            &mut fl,
            MouseEventKind::Drag(MouseButton::Left),
            bar,
            pane.y + pane.height - 2,
        );
        assert_eq!(fl.diff_scroll, 0, "a cancelled grab must not scrub");
    }

    #[test]
    fn diff_rows_cache_is_invalidated_when_the_content_changes() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        fl.diff_area = Rect::new(0, 0, 82, 24);
        fl.set_diff("a\nb\nc\n".repeat(10)); // 30 rows: overflows the viewport
        assert!(fl.ensure_diff_rows(fl.diff_area), "a diff exists");
        let first = fl
            .diff_rows
            .as_ref()
            .expect("cache filled")
            .2
            .index()
            .total();
        assert_eq!(first, 30);
        assert_eq!(
            fl.diff_rows.as_ref().map(|(v, w, _)| (*v, *w)),
            Some((fl.diff_content_version, bordered_inner(fl.diff_area).width)),
            "the index is memoized under (content version, pane width)"
        );
        assert!(fl.ensure_diff_rows(fl.diff_area), "a hit rebuilds nothing");

        // Same width, different content: a stale entry would lie about the row
        // count, so the version bump on set_diff must force a rebuild.
        fl.set_diff("x\n".repeat(60));
        assert!(fl.ensure_diff_rows(fl.diff_area));
        let (v, w, rows) = fl.diff_rows.as_ref().expect("cache refilled");
        assert_eq!(
            (*v, *w),
            (fl.diff_content_version, bordered_inner(fl.diff_area).width)
        );
        assert_ne!(rows.index().total(), first);
        assert_eq!(rows.index().total(), 60);
    }

    #[test]
    fn a_height_only_resize_re_aims_the_index_at_the_new_content_width() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        // 30 rows: fits a 32-row viewport, overflows a 12-row one.
        fl.set_diff("line\n".repeat(30));
        fl.diff_area = Rect::new(0, 0, 82, 34);
        assert!(fl.ensure_diff_rows(fl.diff_area));
        let no_bar = scroll_pane::pane_scroll_layout(
            bordered_inner(fl.diff_area),
            &fl.diff_rows.as_ref().expect("cache").2,
            0,
        );
        assert!(no_bar.bar.is_none());

        // Shorter, same width: the thumb decision flips, and the cached index
        // must be re-aimed at the width beside the thumb -- not served at the
        // stale full width.
        fl.diff_area = Rect::new(0, 0, 82, 14);
        assert!(fl.ensure_diff_rows(fl.diff_area));
        let (v, w, rows) = fl.diff_rows.as_ref().expect("cache reused");
        assert_eq!(
            (*v, *w),
            (fl.diff_content_version, bordered_inner(fl.diff_area).width)
        );
        let layout = scroll_pane::pane_scroll_layout(bordered_inner(fl.diff_area), rows, 0);
        assert!(layout.bar.is_some(), "the thumb appears after the shrink");
        assert_eq!(
            rows.index().index_width(),
            bordered_inner(fl.diff_area).width - 1
        );
        assert_eq!(rows.index().total(), 30);
        assert!(fl.ensure_diff_rows(fl.diff_area), "cache still hits");
    }

    #[test]
    fn diff_rows_cache_tracks_the_content_width() {
        let mut fl = FileList::new(Arc::new(Theme::default()));
        // Long enough to overflow both pane widths, so the thumb column is
        // taken and the index is counted at the width beside it.
        fl.set_diff("word ".repeat(1000));
        fl.diff_area = Rect::new(0, 0, 82, 24);
        assert!(fl.ensure_diff_rows(fl.diff_area));
        let (wide_width, wide_rows) = {
            let (_, w, rows) = fl.diff_rows.as_ref().expect("cache filled");
            (*w, rows.index().total())
        };
        // The same content at another width (a panel resize) must re-key, not
        // serve windows wrapped for the old width.
        fl.diff_area = Rect::new(0, 0, 42, 24);
        assert!(fl.ensure_diff_rows(fl.diff_area));
        let (v, w, rows) = fl.diff_rows.as_ref().expect("cache refilled");
        assert_eq!(*w, bordered_inner(fl.diff_area).width);
        assert_ne!(*w, wide_width, "the pane width is the cache key");
        assert_eq!(*v, fl.diff_content_version);
        // The narrower pane wraps the same words into more rows, and the
        // index counts at the width left beside the thumb column.
        assert!(rows.index().total() > wide_rows);
        assert_eq!(
            rows.index().index_width(),
            bordered_inner(fl.diff_area).width - 1
        );
    }
}
