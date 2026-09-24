use super::test_support::*;
use super::*;
use std::{fs, path::Path};
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
fn test_status_queries_keep_untracked_dirs_compact() {
    let (tmp, _repo) = init_temp_repo();
    fs::create_dir_all(tmp.path().join("nested")).unwrap();
    fs::write(tmp.path().join("nested/file.txt"), "new").unwrap();
    fs::write(tmp.path().join("nested/another.txt"), "new").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.is_dirty);
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, Path::new("nested"));
    assert_eq!(status.files[0].status, FileStatus::Untracked);

    let fetched = query_status_with_fetch(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(fetched.files, status.files);
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
    let (tmp, _source, _repo) = init_repo_with_submodule();
    let inner = Repository::open(tmp.path().join("my-sub")).unwrap();
    let head = inner.head().unwrap().peel_to_commit().unwrap();
    inner.branch("feature", &head, false).unwrap();
    inner.set_head("refs/heads/feature").unwrap();
    fs::write(tmp.path().join("my-sub/lib.rs"), "changed").unwrap();
    fs::write(tmp.path().join("plain.txt"), "ordinary file").unwrap();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let entry = status
        .files
        .iter()
        .find(|f| f.path == Path::new("my-sub"))
        .unwrap();
    assert!(entry.is_submodule);
    assert_eq!(entry.submodule_state, Some(SubmoduleState::Dirty));
    assert!(entry.submodule_warn.is_clean());
    assert_eq!(
        entry.submodule_head,
        Some(SubmoduleHead::Branch("feature".to_string()))
    );

    let plain = status
        .files
        .iter()
        .find(|f| f.path == Path::new("plain.txt"))
        .unwrap();
    assert!(!plain.is_submodule);
    assert_eq!(plain.submodule_state, None);
    assert_eq!(plain.submodule_head, None);
    assert!(plain.submodule_warn.is_clean());
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
fn test_uninitialized_submodule_with_staged_pointer_has_no_head_or_push_warning() {
    let (tmp, _source, _repo) = init_repo_with_submodule();
    add_unpushed_commit_in_sub(tmp.path(), "my-sub");
    stage_submodule_pointer(tmp.path(), "my-sub");
    fs::remove_dir_all(tmp.path().join("my-sub")).unwrap();
    // A deinitialized submodule leaves an empty directory. A missing directory
    // instead represents deletion in libgit2's status model.
    fs::create_dir(tmp.path().join("my-sub")).unwrap();
    let repo = Repository::open(tmp.path()).unwrap();
    let flags = repo
        .submodule_status("my-sub", git2::SubmoduleIgnore::None)
        .unwrap();
    assert!(flags.is_wd_uninitialized());
    assert!(flags.is_index_modified());
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let sub = status
        .submodules
        .iter()
        .find(|s| s.path == Path::new("my-sub"))
        .unwrap();
    assert_eq!(sub.state, Some(SubmoduleState::Uninitialized));
    assert!(sub.warn.is_clean());
    assert_eq!(sub.head, None);
}

#[test]
fn test_submodule_state_priority_dirty_over_modified() {
    let (tmp, _source, _repo) = init_repo_with_submodule();
    add_unpushed_commit_in_sub(tmp.path(), "my-sub");
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty content").unwrap();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let sub = status
        .submodules
        .iter()
        .find(|s| s.path == Path::new("my-sub"))
        .unwrap();
    assert!(sub.head_oid.is_some());
    assert!(sub.workdir_oid.is_some());
    assert_ne!(sub.head_oid, sub.workdir_oid);
    assert_eq!(sub.state, Some(SubmoduleState::Dirty));
}

#[test]
fn test_default_branch_name_uses_non_origin_remote() {
    // Gerrit/mirror workspace: no origin, remote named `gerrit`.
    let (_tmp, repo) = init_temp_repo();
    repo.remote("gerrit", "https://example.com/repo.git")
        .unwrap();
    let oid = repo.head().unwrap().target().unwrap();
    repo.reference("refs/remotes/gerrit/main", oid, true, "test")
        .unwrap();
    assert_eq!(default_branch_name(&repo).as_deref(), Some("gerrit/main"));
}
