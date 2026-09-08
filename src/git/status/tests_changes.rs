use super::test_support::{commit_index, init_temp_repo};
use super::*;

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
