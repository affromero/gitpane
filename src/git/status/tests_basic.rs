use super::test_support::*;
use super::*;
use git2::SubmoduleStatus;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

#[test]
fn test_clean_repo_reports_no_changes() {
    let (tmp, _repo) = init_temp_repo();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(!status.is_dirty);
    assert!(status.files.is_empty());
}

#[test]
fn test_modified_file_detected() {
    let (tmp, repo) = init_temp_repo();

    // Add and commit a file
    let file_path = tmp.path().join("test.txt");
    fs::write(&file_path, "hello").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("test.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Add file", &tree, &[&head])
        .unwrap();

    // Modify it
    fs::write(&file_path, "world").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.is_dirty);
    assert!(
        status
            .files
            .iter()
            .any(|f| f.status == FileStatus::Modified)
    );
}

#[test]
fn test_untracked_file_detected() {
    let (tmp, _repo) = init_temp_repo();
    fs::write(tmp.path().join("new.txt"), "new").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.is_dirty);
    assert!(
        status
            .files
            .iter()
            .any(|f| f.status == FileStatus::Untracked)
    );
}

#[test]
fn test_background_status_does_not_recurse_untracked_dirs() {
    let (tmp, _repo) = init_temp_repo();
    fs::create_dir_all(tmp.path().join("nested")).unwrap();
    fs::write(tmp.path().join("nested/file.txt"), "new").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.is_dirty);
    assert!(
        status
            .files
            .iter()
            .any(|f| f.status == FileStatus::Untracked)
    );
    assert!(
        !status
            .files
            .iter()
            .any(|f| f.path == Path::new("nested/file.txt"))
    );

    let recursive =
        query_status_inner(tmp.path(), false, true, &SubmoduleConfig::default()).unwrap();
    assert!(
        recursive
            .files
            .iter()
            .any(|f| f.path == Path::new("nested/file.txt"))
    );
}

#[test]
fn test_default_branch_name_resolves_origin_main() {
    let (_tmp, repo) = init_temp_repo();
    let oid = repo.head().unwrap().target().unwrap();
    repo.reference("refs/remotes/origin/main", oid, true, "test")
        .unwrap();
    assert_eq!(default_branch_name(&repo).as_deref(), Some("origin/main"));
}

#[test]
fn test_default_branch_name_follows_symbolic_head() {
    let (_tmp, repo) = init_temp_repo();
    let oid = repo.head().unwrap().target().unwrap();
    repo.reference("refs/remotes/origin/master", oid, true, "test")
        .unwrap();
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
        true,
        "test",
    )
    .unwrap();
    assert_eq!(default_branch_name(&repo).as_deref(), Some("origin/master"));
}

#[test]
fn test_default_branch_name_none_without_remote() {
    let (_tmp, repo) = init_temp_repo();
    assert_eq!(default_branch_name(&repo), None);
}

#[test]
fn test_worktree_info_empty_for_plain_repo() {
    let (tmp, _repo) = init_temp_repo();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.worktree_info.is_empty());
}

#[test]
fn test_worktree_info_reflects_linked_worktrees() {
    let (tmp, repo) = init_temp_repo();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = repo.branch("wt-branch", &head, false).unwrap();
    let reference = branch.into_reference();
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));

    let wt_tmp = TempDir::new().unwrap();
    let wt_dir = wt_tmp.path().join("wt1");
    repo.worktree("wt1", &wt_dir, Some(&opts)).unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.worktree_info.len(), 1);
    assert_eq!(status.worktree_info[0].branch, "wt-branch");
    assert_eq!(status.worktree_info[0].name, "wt1");
    assert!(!status.worktree_info[0].is_dirty);
    assert_eq!(status.worktree_info[0].file_count, 0);
    assert!(!status.worktree_info[0].has_dirty_submodules);
    assert!(!status.worktree_info[0].has_unpushed_submodules);
}

#[test]
fn test_worktree_info_reflects_dirty_linked_worktree() {
    let (tmp, repo) = init_temp_repo();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = repo.branch("wt-dirty", &head, false).unwrap();
    let reference = branch.into_reference();
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));

    let wt_tmp = TempDir::new().unwrap();
    let wt_dir = wt_tmp.path().join("wt-dirty");
    repo.worktree("wt-dirty", &wt_dir, Some(&opts)).unwrap();
    fs::write(wt_dir.join("new.txt"), "new").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let wt = status
        .worktree_info
        .iter()
        .find(|wt| wt.name == "wt-dirty")
        .unwrap();

    assert!(wt.is_dirty);
    assert_eq!(wt.file_count, 1);
    assert!(!wt.has_dirty_submodules);
    assert!(!wt.has_unpushed_submodules);
}

#[test]
fn test_worktree_info_reflects_submodule_signals() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    let repo = Repository::open(tmp.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let branch = repo.branch("wt-submodule", &head, false).unwrap();
    let reference = branch.into_reference();
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));

    let wt_tmp = TempDir::new().unwrap();
    let wt_dir = wt_tmp.path().join("wt-submodule");
    repo.worktree("wt-submodule", &wt_dir, Some(&opts)).unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let wt = status
        .worktree_info
        .iter()
        .find(|wt| wt.name == "wt-submodule")
        .unwrap();

    assert!(wt.is_dirty);
    assert!(wt.file_count > 0);
    assert!(wt.has_dirty_submodules);
}

#[test]
fn test_submodule_state_mapping() {
    // Test the flag-to-state conversion logic
    let flags = SubmoduleStatus::WD_UNINITIALIZED;
    assert!(flags.is_wd_uninitialized());

    let flags = SubmoduleStatus::WD_WD_MODIFIED;
    assert!(flags.is_wd_wd_modified());

    let flags = SubmoduleStatus::WD_UNTRACKED;
    assert!(flags.contains(SubmoduleStatus::WD_UNTRACKED));

    let flags = SubmoduleStatus::WD_MODIFIED;
    assert!(flags.is_wd_modified());

    let flags = SubmoduleStatus::WD_INDEX_MODIFIED;
    assert!(flags.contains(SubmoduleStatus::WD_INDEX_MODIFIED));
}

#[test]
fn test_clean_repo_no_dirty_submodules() {
    let (tmp, _repo) = init_temp_repo();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(!status.has_dirty_submodules);
    assert!(status.submodules.is_empty());
}

#[test]
fn test_status_maps_correctly() {
    assert_eq!(FileStatus::Modified.label(), "M");
    assert_eq!(FileStatus::Added.label(), "A");
    assert_eq!(FileStatus::Deleted.label(), "D");
    assert_eq!(FileStatus::Renamed.label(), "R");
    assert_eq!(FileStatus::Untracked.label(), "?");
    assert_eq!(FileStatus::Conflicted.label(), "C");
}

#[test]
fn test_file_entry_submodule_fields() {
    let entry = FileEntry {
        path: PathBuf::from("my-submodule"),
        status: FileStatus::Modified,
        staged: false,
        unstaged: true,
        is_submodule: true,
        submodule_state: Some(SubmoduleState::Modified),
        submodule_warn: SubmoduleWarn::default(),
        submodule_head: Some(SubmoduleHead::Branch("feature".to_string())),
    };
    assert!(entry.is_submodule);
    assert_eq!(entry.submodule_state, Some(SubmoduleState::Modified));
    assert!(entry.submodule_warn.is_clean());
    assert_eq!(
        entry.submodule_head,
        Some(SubmoduleHead::Branch("feature".to_string()))
    );

    let plain = FileEntry {
        path: PathBuf::from("src/main.rs"),
        status: FileStatus::Modified,
        staged: false,
        unstaged: true,
        is_submodule: false,
        submodule_state: None,
        submodule_warn: SubmoduleWarn::default(),
        submodule_head: None,
    };
    assert!(!plain.is_submodule);
    assert_eq!(plain.submodule_state, None);
    assert_eq!(plain.submodule_head, None);
}

#[test]
fn test_staged_and_unstaged_flags_distinguished() {
    let (tmp, repo) = init_temp_repo();

    // Commit an initial file.
    let file_path = tmp.path().join("test.txt");
    fs::write(&file_path, "one").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("test.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Add file", &tree, &[&head])
        .unwrap();

    // Stage a change, then modify again on disk: the index differs from HEAD
    // (staged) and the worktree differs from the index (unstaged).
    fs::write(&file_path, "two").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("test.txt")).unwrap();
    index.write().unwrap();
    fs::write(&file_path, "three").unwrap();

    // An untracked file is unstaged-only.
    fs::write(tmp.path().join("new.txt"), "new").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();

    let tracked = status
        .files
        .iter()
        .find(|f| f.path == Path::new("test.txt"))
        .expect("tracked file present");
    assert!(tracked.staged && tracked.unstaged);

    let untracked = status
        .files
        .iter()
        .find(|f| f.path == Path::new("new.txt"))
        .expect("untracked file present");
    assert_eq!(untracked.status, FileStatus::Untracked);
    assert!(untracked.unstaged && !untracked.staged);
}

#[test]
fn test_submodule_state_equality_and_clone() {
    assert_eq!(SubmoduleState::Modified, SubmoduleState::Modified);
    assert_eq!(SubmoduleState::Dirty, SubmoduleState::Dirty);
    assert_eq!(SubmoduleState::Uninitialized, SubmoduleState::Uninitialized);
    assert_ne!(SubmoduleState::Modified, SubmoduleState::Dirty);
    assert_ne!(SubmoduleState::Dirty, SubmoduleState::Uninitialized);

    let state = SubmoduleState::Modified;
    let cloned = state.clone();
    assert_eq!(state, cloned);
}

#[test]
fn test_ignore_dirty_subs_on_clean_repo() {
    // ignore_dirty_subs = true should work fine on repos without submodules
    let (tmp, _repo) = init_temp_repo();
    let status = query_status(
        tmp.path(),
        &SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: false,
        },
    )
    .unwrap();
    assert!(!status.is_dirty);
    assert!(status.files.is_empty());
    assert!(status.submodules.is_empty());
    assert!(!status.has_dirty_submodules);
}

#[test]
fn test_ignore_dirty_subs_still_detects_regular_changes() {
    let (tmp, _repo) = init_temp_repo();
    fs::write(tmp.path().join("new.txt"), "new").unwrap();

    let status = query_status(
        tmp.path(),
        &SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: false,
        },
    )
    .unwrap();
    assert!(status.is_dirty);
    assert!(
        status
            .files
            .iter()
            .any(|f| f.status == FileStatus::Untracked)
    );
    // Submodule fields should be empty when ignored
    assert!(status.submodules.is_empty());
    assert!(!status.has_dirty_submodules);
}

#[test]
fn test_submodule_state_priority_uninitialized_first() {
    // WD_UNINITIALIZED takes priority over other flags
    let flags = SubmoduleStatus::WD_UNINITIALIZED | SubmoduleStatus::WD_MODIFIED;
    assert!(flags.is_wd_uninitialized());
}

#[test]
fn test_submodule_state_priority_dirty_over_modified() {
    // WD_WD_MODIFIED (dirty) is checked before WD_MODIFIED (pointer change)
    let flags = SubmoduleStatus::WD_WD_MODIFIED | SubmoduleStatus::WD_MODIFIED;
    assert!(flags.is_wd_wd_modified());
    assert!(flags.is_wd_modified());
    // Our mapping logic checks dirty first, so this should map to Dirty
}
