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
