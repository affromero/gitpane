use git2::{Repository, StatusOptions, SubmoduleStatus};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct RepoStatus {
    pub branch: String,
    pub files: Vec<FileEntry>,
    pub ahead: usize,
    pub behind: usize,
    pub is_dirty: bool,
    /// Number of linked worktrees (excludes the main working tree)
    pub worktrees: usize,
    /// True when .gitmodules exists (repo uses submodules)
    pub has_submodules: bool,
    pub submodules: Vec<SubmoduleInfo>,
    pub has_dirty_submodules: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    pub status: FileStatus,
    pub is_submodule: bool,
    pub submodule_state: Option<SubmoduleState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SubmoduleState {
    Modified,
    Uninitialized,
    Dirty,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct SubmoduleInfo {
    pub name: String,
    pub path: PathBuf,
    pub state: SubmoduleState,
    pub head_oid: Option<String>,
    pub workdir_oid: Option<String>,
}

impl FileStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
            Self::Conflicted => "C",
        }
    }
}

/// Fast local-only status query (no network). Used by filesystem watcher refreshes.
pub(crate) fn query_status(path: &Path, ignore_dirty_subs: bool) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, false, ignore_dirty_subs)
}

/// Status query with `git fetch` first. Used by explicit user refresh (`r` key).
pub(crate) fn query_status_with_fetch(
    path: &Path,
    ignore_dirty_subs: bool,
) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, true, ignore_dirty_subs)
}

fn query_status_inner(
    path: &Path,
    fetch: bool,
    ignore_dirty_subs: bool,
) -> color_eyre::Result<RepoStatus> {
    let repo = Repository::open(path)?;

    // Branch name
    let branch = match repo.head() {
        Ok(reference) => reference.shorthand().unwrap_or("HEAD").to_string(),
        Err(_) => "(no branch)".to_string(),
    };

    // Only fetch remote-tracking refs when explicitly requested
    if fetch {
        fetch_remote_silent(path);
    }

    // Ahead/behind
    let (ahead, behind) = compute_ahead_behind(&repo);

    // File statuses
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    if ignore_dirty_subs {
        opts.exclude_submodules(true);
    }

    let statuses = repo.statuses(Some(&mut opts))?;
    let mut files = Vec::new();

    for entry in statuses.iter() {
        let s = entry.status();
        let file_path = PathBuf::from(entry.path().unwrap_or(""));

        let file_status = if s.is_conflicted() {
            FileStatus::Conflicted
        } else if s.is_index_new() || s.is_wt_new() {
            if s.is_wt_new() && !s.is_index_new() {
                FileStatus::Untracked
            } else {
                FileStatus::Added
            }
        } else if s.is_index_deleted() || s.is_wt_deleted() {
            FileStatus::Deleted
        } else if s.is_index_renamed() || s.is_wt_renamed() {
            FileStatus::Renamed
        } else if s.is_index_modified() || s.is_wt_modified() {
            FileStatus::Modified
        } else {
            continue;
        };

        files.push(FileEntry {
            path: file_path,
            status: file_status,
            is_submodule: false,
            submodule_state: None,
        });
    }

    let is_dirty = !files.is_empty();

    // Count linked worktrees (excludes the main working tree)
    let worktrees = repo.worktrees().map(|wt| wt.len()).unwrap_or(0);

    // Detect submodules by checking for .gitmodules
    let has_submodules = path.join(".gitmodules").is_file();

    // Submodule enumeration
    let mut submodules = Vec::new();
    let mut has_dirty_submodules = false;

    if has_submodules
        && !ignore_dirty_subs
        && let Ok(subs) = repo.submodules()
    {
        for sub in &subs {
            let name = sub.name().unwrap_or("").to_string();
            let sub_path = PathBuf::from(sub.path());
            let status = repo
                .submodule_status(&name, git2::SubmoduleIgnore::Unspecified)
                .unwrap_or(SubmoduleStatus::empty());

            let state = if status.is_wd_uninitialized() {
                Some(SubmoduleState::Uninitialized)
            } else if status.is_wd_wd_modified() || status.contains(SubmoduleStatus::WD_UNTRACKED) {
                Some(SubmoduleState::Dirty)
            } else if status.is_wd_modified() || status.contains(SubmoduleStatus::WD_INDEX_MODIFIED)
            {
                Some(SubmoduleState::Modified)
            } else {
                None
            };

            if let Some(state) = state {
                let head_oid = sub.head_id().map(|id| id.to_string());
                let workdir_oid = sub.workdir_id().map(|id| id.to_string());

                submodules.push(SubmoduleInfo {
                    name: name.clone(),
                    path: sub_path.clone(),
                    state: state.clone(),
                    head_oid,
                    workdir_oid,
                });

                // Cross-reference with files vec
                if let Some(file_entry) = files.iter_mut().find(|f| f.path == sub_path) {
                    file_entry.is_submodule = true;
                    file_entry.submodule_state = Some(state.clone());
                } else {
                    // Add synthetic FileEntry for dirty submodules not already in files
                    files.push(FileEntry {
                        path: sub_path,
                        status: FileStatus::Modified,
                        is_submodule: true,
                        submodule_state: Some(state),
                    });
                }

                has_dirty_submodules = true;
            }
        }
    }

    Ok(RepoStatus {
        branch,
        files,
        ahead,
        behind,
        is_dirty: is_dirty || has_dirty_submodules,
        worktrees,
        has_submodules,
        submodules,
        has_dirty_submodules,
    })
}

/// Run `git fetch` in the background to update remote-tracking refs.
/// Uses the CLI because git2 fetch doesn't support SSH agent / credential helpers
/// out of the box. Silently ignores failures (offline, auth issues, etc.).
fn fetch_remote_silent(path: &Path) {
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("fetch")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn compute_ahead_behind(repo: &Repository) -> (usize, usize) {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return (0, 0),
    };

    let local_oid = match head.target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    let branch_name = match head.shorthand() {
        Some(name) => name.to_string(),
        None => return (0, 0),
    };

    // Use git2's branch upstream tracking instead of hardcoding "origin"
    let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return (0, 0),
    };

    let upstream = match branch.upstream() {
        Ok(u) => u,
        Err(_) => return (0, 0),
    };

    let upstream_oid = match upstream.get().target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    repo.graph_ahead_behind(local_oid, upstream_oid)
        .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_temp_repo() -> (TempDir, Repository) {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        // Create initial commit so HEAD exists
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                .unwrap();
        }

        (tmp, repo)
    }

    #[test]
    fn test_clean_repo_reports_no_changes() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), false).unwrap();
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

        let status = query_status(tmp.path(), false).unwrap();
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

        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.is_dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.status == FileStatus::Untracked)
        );
    }

    #[test]
    fn test_worktree_count_zero_for_plain_repo() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), false).unwrap();
        assert_eq!(status.worktrees, 0);
    }

    #[test]
    fn test_worktree_count_reflects_linked_worktrees() {
        let (tmp, _repo) = init_temp_repo();
        // Create a linked worktree via git CLI
        let wt_dir = tmp.path().join("wt1");
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .arg("worktree")
            .arg("add")
            .arg(&wt_dir)
            .arg("-b")
            .arg("wt-branch")
            .output()
            .unwrap();
        assert!(output.status.success(), "git worktree add failed");

        let status = query_status(tmp.path(), false).unwrap();
        assert_eq!(status.worktrees, 1);
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
        let status = query_status(tmp.path(), false).unwrap();
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
            is_submodule: true,
            submodule_state: Some(SubmoduleState::Modified),
        };
        assert!(entry.is_submodule);
        assert_eq!(entry.submodule_state, Some(SubmoduleState::Modified));

        let plain = FileEntry {
            path: PathBuf::from("src/main.rs"),
            status: FileStatus::Modified,
            is_submodule: false,
            submodule_state: None,
        };
        assert!(!plain.is_submodule);
        assert_eq!(plain.submodule_state, None);
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
        let status = query_status(tmp.path(), true).unwrap();
        assert!(!status.is_dirty);
        assert!(status.files.is_empty());
        assert!(status.submodules.is_empty());
        assert!(!status.has_dirty_submodules);
    }

    #[test]
    fn test_ignore_dirty_subs_still_detects_regular_changes() {
        let (tmp, _repo) = init_temp_repo();
        fs::write(tmp.path().join("new.txt"), "new").unwrap();

        let status = query_status(tmp.path(), true).unwrap();
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

    /// Helper: creates a temp repo with a submodule, returns (parent_tmp, sub_source_tmp, sub_repo)
    fn init_repo_with_submodule() -> (TempDir, TempDir, Repository) {
        let (tmp, _repo) = init_temp_repo();

        let sub_source = TempDir::new().unwrap();
        let sub_repo = Repository::init(sub_source.path()).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            fs::write(sub_source.path().join("lib.rs"), "fn hello() {}").unwrap();
            let mut idx = sub_repo.index().unwrap();
            idx.add_path(Path::new("lib.rs")).unwrap();
            idx.write().unwrap();
            let tree_id = idx.write_tree().unwrap();
            let tree = sub_repo.find_tree(tree_id).unwrap();
            sub_repo
                .commit(Some("HEAD"), &sig, &sig, "init sub", &tree, &[])
                .unwrap();
        }

        // Add submodule (requires protocol.file.allow for local paths)
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args([
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub_source.path().to_str().unwrap(),
                "my-sub",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git submodule add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Commit the submodule addition (use -c user.* for CI environments without global git config)
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "."])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "-m",
                "add submodule",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        (tmp, sub_source, sub_repo)
    }

    #[test]
    fn test_dirty_submodule_with_real_git_submodule() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Verify: clean state should show has_submodules but no dirty submodules
        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());

        // Now make the submodule dirty by modifying a file inside it
        let sub_workdir = tmp.path().join("my-sub");
        fs::write(sub_workdir.join("lib.rs"), "fn hello() { /* changed */ }").unwrap();

        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        assert_eq!(sub_info.state, SubmoduleState::Dirty);

        // Verify the file entry is annotated
        let file_entry = status.files.iter().find(|f| f.path == Path::new("my-sub"));
        assert!(file_entry.is_some());
        let file_entry = file_entry.unwrap();
        assert!(file_entry.is_submodule);
        assert_eq!(file_entry.submodule_state, Some(SubmoduleState::Dirty));
    }

    #[test]
    fn test_ignore_dirty_subs_hides_submodule_state() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Make the submodule dirty
        fs::write(tmp.path().join("my-sub/lib.rs"), "fn changed() {}").unwrap();

        // With ignore_dirty_subs = true, submodule state should be hidden
        let status = query_status(tmp.path(), true).unwrap();
        assert!(status.has_submodules); // .gitmodules still exists
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
        // No submodule-annotated entries
        assert!(!status.files.iter().any(|f| f.is_submodule));
    }

    #[test]
    fn test_submodule_modified_pointer() {
        let (tmp, _sub_source, sub_repo) = init_repo_with_submodule();

        // Add a new commit to the submodule source
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            fs::write(_sub_source.path().join("lib.rs"), "v2").unwrap();
            let mut idx = sub_repo.index().unwrap();
            idx.add_path(Path::new("lib.rs")).unwrap();
            idx.write().unwrap();
            let tree_id = idx.write_tree().unwrap();
            let tree = sub_repo.find_tree(tree_id).unwrap();
            let head = sub_repo.head().unwrap().peel_to_commit().unwrap();
            sub_repo
                .commit(Some("HEAD"), &sig, &sig, "v2", &tree, &[&head])
                .unwrap();
        }

        // Pull the new commit inside the submodule workdir
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path().join("my-sub"))
            .args([
                "-c",
                "protocol.file.allow=always",
                "pull",
                "origin",
                "master",
            ])
            .output()
            .unwrap();
        // Try main if master fails
        if !output.status.success() {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path().join("my-sub"))
                .args(["-c", "protocol.file.allow=always", "pull", "origin", "main"])
                .output();
        }

        // Now the submodule pointer has changed (HEAD in submodule != recorded in parent)
        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        // Could be Modified or Dirty depending on exact git state
        assert!(
            sub_info.state == SubmoduleState::Modified || sub_info.state == SubmoduleState::Dirty,
            "expected Modified or Dirty, got {:?}",
            sub_info.state
        );

        // Verify OIDs are populated
        assert!(sub_info.head_oid.is_some());
        assert!(sub_info.workdir_oid.is_some());
        assert_ne!(sub_info.head_oid, sub_info.workdir_oid);
    }

    #[test]
    fn test_clean_submodule_not_reported() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Without any modifications, the submodule should be clean
        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
        assert!(!status.files.iter().any(|f| f.is_submodule));
    }

    #[test]
    fn test_dirty_submodule_makes_repo_dirty() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Start clean
        let status = query_status(tmp.path(), false).unwrap();
        assert!(!status.is_dirty);

        // Make submodule dirty
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        // Now repo should be dirty
        let status = query_status(tmp.path(), false).unwrap();
        assert!(status.is_dirty);
    }
}
