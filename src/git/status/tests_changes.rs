use super::test_support::{
    add_commit_on_head, commit_index, init_repo_with_submodule, init_temp_repo,
    stage_submodule_pointer,
};
use super::*;

#[test]
fn nested_child_observations_preserve_git_ignore_and_state_rules() {
    use git2::{SubmoduleIgnore, SubmoduleStatus};
    let mask = SubmoduleStatus::WD_ADDED
        | SubmoduleStatus::WD_DELETED
        | SubmoduleStatus::WD_MODIFIED
        | SubmoduleStatus::WD_INDEX_MODIFIED
        | SubmoduleStatus::WD_WD_MODIFIED
        | SubmoduleStatus::WD_UNTRACKED;
    for (nested_ignore, diff_ignore) in [
        ("none", None),
        ("untracked", None),
        ("dirty", None),
        ("all", None),
        ("none", Some("untracked")),
        ("none", Some("dirty")),
        ("none", Some("all")),
    ] {
        for scenario in [
            "untracked",
            "working",
            "staged",
            "pointer",
            "missing",
            "uninitialized",
            "removed-from-index",
        ] {
            let (tmp, _source, _repo) = init_repo_with_submodule();
            let parent = Repository::open(tmp.path()).unwrap();
            let child = Repository::open(tmp.path().join("my-sub")).unwrap();
            if let Some(value) = diff_ignore {
                child
                    .config()
                    .unwrap()
                    .set_str("diff.ignoreSubmodules", value)
                    .unwrap();
            }
            let (source, nested) = init_temp_repo();
            add_commit_on_head(&nested, "nested.txt", "initial");
            let mut link = child
                .submodule(source.path().to_str().unwrap(), Path::new("nested"), true)
                .unwrap();
            link.clone(None).unwrap();
            link.add_finalize().unwrap();
            drop(link);
            git2::Config::open(&tmp.path().join("my-sub/.gitmodules"))
                .unwrap()
                .set_str("submodule.nested.ignore", nested_ignore)
                .unwrap();
            let mut index = child.index().unwrap();
            index.add_path(Path::new(".gitmodules")).unwrap();
            index.write().unwrap();
            commit_index(&child, "Add nested submodule");
            stage_submodule_pointer(tmp.path(), "my-sub");
            commit_index(&parent, "Record nested child");
            let recorded = parent
                .index()
                .unwrap()
                .get_path(Path::new("my-sub"), 0)
                .unwrap()
                .id;
            let nested_path = tmp.path().join("my-sub/nested");
            match scenario {
                "untracked" => std::fs::write(nested_path.join("new.txt"), "new").unwrap(),
                "working" | "staged" => {
                    std::fs::write(nested_path.join("nested.txt"), "changed").unwrap();
                    if scenario == "staged" {
                        let inner = Repository::open(&nested_path).unwrap();
                        let mut index = inner.index().unwrap();
                        index.add_path(Path::new("nested.txt")).unwrap();
                        index.write().unwrap();
                    }
                }
                "pointer" => {
                    add_commit_on_head(&Repository::open(&nested_path).unwrap(), "new.txt", "new");
                }
                "removed-from-index" => {
                    let mut index = child.index().unwrap();
                    index.remove_path(Path::new("nested")).unwrap();
                    index.write().unwrap();
                }
                "missing" | "uninitialized" => {
                    std::fs::remove_dir_all(&nested_path).unwrap();
                    if scenario == "uninitialized" {
                        std::fs::create_dir(&nested_path).unwrap();
                    }
                }
                _ => unreachable!(),
            }
            for ignore in [
                SubmoduleIgnore::None,
                SubmoduleIgnore::Untracked,
                SubmoduleIgnore::Dirty,
                SubmoduleIgnore::All,
            ] {
                let expected = parent.submodule_status("my-sub", ignore).unwrap();
                let observed =
                    super::submodule::observe_child(&child, Some(recorded), ignore).unwrap();
                assert_eq!(
                    observed.status & mask,
                    expected & mask,
                    "{scenario} {ignore:?} nested={nested_ignore} diff={diff_ignore:?}"
                );
            }
        }
    }
}

#[test]
fn working_submodule_records_follow_repository_diff_ignore_configuration() {
    for registration_ignore in ["none", "untracked", "dirty", "all"] {
        for ignore in ["none", "untracked", "dirty", "all", "invalid", "valueless"] {
            for scenario in ["working", "untracked", "pointer"] {
                let (tmp, _source, _child) = init_repo_with_submodule();
                let repo = Repository::open(tmp.path()).unwrap();
                git2::Config::open(&tmp.path().join(".gitmodules"))
                    .unwrap()
                    .set_str("submodule.my-sub.ignore", registration_ignore)
                    .unwrap();
                if ignore == "valueless" {
                    let config = repo.path().join("config");
                    let original = std::fs::read_to_string(&config).unwrap();
                    std::fs::write(config, format!("{original}\n[diff]\nignoreSubmodules\n"))
                        .unwrap();
                } else {
                    repo.config()
                        .unwrap()
                        .set_str("diff.ignoreSubmodules", ignore)
                        .unwrap();
                }
                match scenario {
                    "working" => {
                        std::fs::write(tmp.path().join("my-sub/lib.rs"), "changed").unwrap()
                    }
                    "untracked" => std::fs::write(tmp.path().join("my-sub/new.rs"), "new").unwrap(),
                    "pointer" => {
                        add_commit_on_head(
                            &Repository::open(tmp.path().join("my-sub")).unwrap(),
                            "new.rs",
                            "new",
                        );
                    }
                    _ => unreachable!(),
                }
                let config = SubmoduleConfig {
                    ignore_dirty: false,
                    warn_unpushed: false,
                };
                let mut options = super::change_summary::status_options(true, &config);
                let expected = repo
                    .statuses(Some(&mut options))
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry.path().is_ok_and(|path| path == "my-sub")
                            && entry.status().is_wt_modified()
                    });
                let records =
                    serde_json::to_value(collect_changes(&repo, &config, 100).unwrap()).unwrap();
                let actual = records
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|record| record["path"] == "my-sub" && record["group"] == "working");
                assert_eq!(
                    actual, expected,
                    "registration={registration_ignore} ignore={ignore} scenario={scenario}: {records}"
                );
            }
        }
    }
}

#[test]
fn compact_gitlink_changes_match_git_after_removal_from_index_or_config() {
    for remove_config in [false, true] {
        for checkout in ["initialized", "missing", "uninitialized", "file"] {
            let (tmp, _source, _child) = init_repo_with_submodule();
            let repo = Repository::open(tmp.path()).unwrap();
            let mut index = repo.index().unwrap();
            index.remove_path(Path::new("my-sub")).unwrap();
            index.write().unwrap();
            if remove_config {
                std::fs::remove_file(tmp.path().join(".gitmodules")).unwrap();
            }
            if checkout != "initialized" {
                std::fs::remove_dir_all(tmp.path().join("my-sub")).unwrap();
                if checkout == "uninitialized" {
                    std::fs::create_dir(tmp.path().join("my-sub")).unwrap();
                }
                if checkout == "file" {
                    std::fs::write(tmp.path().join("my-sub"), "replacement").unwrap();
                }
            }
            let config = SubmoduleConfig {
                ignore_dirty: false,
                warn_unpushed: false,
            };
            let mut options = super::change_summary::status_options(false, &config);
            let expected: Vec<_> = repo
                .statuses(Some(&mut options))
                .unwrap()
                .iter()
                .map(|entry| {
                    let path = entry
                        .index_to_workdir()
                        .or_else(|| entry.head_to_index())
                        .unwrap()
                        .new_file()
                        .path()
                        .unwrap()
                        .to_path_buf();
                    let file = super::change_summary::file_entry(path, entry.status()).unwrap();
                    (file.path, file.status, file.staged, file.unstaged)
                })
                .collect();
            let actual = query_status(tmp.path(), &config).unwrap();
            let mut actual: Vec<_> = actual
                .files
                .into_iter()
                .map(|file| (file.path, file.status, file.staged, file.unstaged))
                .collect();
            let mut expected = expected;
            actual.sort_by(|a, b| a.0.cmp(&b.0));
            expected.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(
                actual, expected,
                "remove_config={remove_config} checkout={checkout}"
            );
        }
    }
}

#[test]
fn staged_rename_keeps_its_working_edit_or_deletion_as_a_separate_change() {
    for deleted in [false, true] {
        let (tmp, repo) = init_temp_repo();
        std::fs::write(tmp.path().join("old"), "original\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("old")).unwrap();
        index.write().unwrap();
        commit_index(&repo, "Original file");
        std::fs::rename(tmp.path().join("old"), tmp.path().join("new")).unwrap();
        index.remove_path(Path::new("old")).unwrap();
        index.add_path(Path::new("new")).unwrap();
        index.write().unwrap();
        if deleted {
            std::fs::remove_file(tmp.path().join("new")).unwrap();
        } else {
            std::fs::write(tmp.path().join("new"), "working edit\n").unwrap();
        }
        let records =
            serde_json::to_value(collect_changes(&repo, &SubmoduleConfig::default(), 100).unwrap())
                .unwrap();
        let records = records.as_array().unwrap();
        assert_eq!(records.len(), 2);
        let staged = records
            .iter()
            .find(|record| record["group"] == "index")
            .unwrap();
        assert_eq!(staged["path"], "new");
        assert_eq!(staged["kind"], "index_renamed");
        assert_eq!(staged["original_path"], "old");
        let working = records
            .iter()
            .find(|record| record["group"] == "working")
            .unwrap();
        assert_eq!(working["path"], "new");
        assert_eq!(
            working["kind"],
            if deleted {
                "working_deleted"
            } else {
                "working_modified"
            }
        );
    }
}

#[test]
fn bare_repository_reports_head_without_worktree_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = Repository::init_bare(tmp.path()).unwrap();
    let oid = commit_index(&repo, "bare commit");
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.head_oid, Some(oid.to_string()));
    assert!(!status.is_dirty);
    assert!(status.files.is_empty());
}

#[cfg(unix)]
#[test]
fn file_type_changes_remain_visible_before_and_after_staging() {
    let (tmp, repo) = init_temp_repo();
    std::fs::write(tmp.path().join("file"), "original").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.write().unwrap();
    commit_index(&repo, "regular file");
    std::fs::remove_file(tmp.path().join("file")).unwrap();
    std::os::unix::fs::symlink("target", tmp.path().join("file")).unwrap();
    for staged in [false, true] {
        if staged {
            index.add_path(Path::new("file")).unwrap();
            index.write().unwrap();
        }
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
        assert_eq!(status.files.len(), 1);
        assert_eq!(status.files[0].status, FileStatus::Modified);
        assert_eq!(status.files[0].staged, staged);
        assert_eq!(status.files[0].unstaged, !staged);
    }
}

#[test]
fn staged_rename_displays_its_current_path() {
    let (tmp, repo) = init_temp_repo();
    std::fs::write(tmp.path().join("old"), "original").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("old")).unwrap();
    index.write().unwrap();
    commit_index(&repo, "original");
    std::fs::rename(tmp.path().join("old"), tmp.path().join("new")).unwrap();
    index.remove_path(Path::new("old")).unwrap();
    index.add_path(Path::new("new")).unwrap();
    index.write().unwrap();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, Path::new("new"));
    assert_eq!(status.files[0].status, FileStatus::Renamed);
}
