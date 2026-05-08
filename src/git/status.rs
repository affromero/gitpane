use crate::config::SubmoduleConfig;
use git2::{Repository, StatusOptions, SubmoduleStatus};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct RepoStatus {
    pub branch: String,
    pub files: Vec<FileEntry>,
    pub ahead: usize,
    pub behind: usize,
    pub is_dirty: bool,
    /// Linked worktrees (excludes the main working tree)
    pub worktree_info: Vec<WorktreeEntry>,
    /// True when .gitmodules exists (repo uses submodules)
    pub has_submodules: bool,
    pub submodules: Vec<SubmoduleInfo>,
    pub has_dirty_submodules: bool,
    /// Any submodule has unpushed_commits>0 OR pointer_unreachable
    pub has_unpushed_submodules: bool,
    /// True when the last `git fetch` failed (auth, network, timeout)
    pub fetch_failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    pub status: FileStatus,
    pub is_submodule: bool,
    pub submodule_state: Option<SubmoduleState>,
    pub submodule_warn: SubmoduleWarn,
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

/// "You owe a push" warnings for a submodule. Orthogonal to `SubmoduleState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubmoduleWarn {
    /// Submodule HEAD branch ahead of its upstream (0 if no upstream).
    pub unpushed_commits: usize,
    /// Parent's recorded oid is not reachable from any of the submodule's
    /// `refs/remotes/*` refs — committing the parent would pin a sha nobody
    /// else can fetch.
    pub pointer_unreachable: bool,
}

impl SubmoduleWarn {
    pub fn is_clean(&self) -> bool {
        self.unpushed_commits == 0 && !self.pointer_unreachable
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct WorktreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct SubmoduleInfo {
    pub name: String,
    pub path: PathBuf,
    pub state: Option<SubmoduleState>,
    pub head_oid: Option<String>,
    pub workdir_oid: Option<String>,
    pub warn: SubmoduleWarn,
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
pub(crate) fn query_status(
    path: &Path,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, false, sub_cfg)
}

/// Status query with `git fetch` first. Used by explicit user refresh (`r` key).
pub(crate) fn query_status_with_fetch(
    path: &Path,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, true, sub_cfg)
}

fn query_status_inner(
    path: &Path,
    fetch: bool,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<RepoStatus> {
    let repo = Repository::open(path)?;

    // Branch name
    let branch = match repo.head() {
        Ok(reference) => reference.shorthand().unwrap_or("HEAD").to_string(),
        Err(_) => "(no branch)".to_string(),
    };

    // Only fetch remote-tracking refs when explicitly requested
    let fetch_failed = if fetch {
        !fetch_remote_silent(path)
    } else {
        false
    };

    // Ahead/behind
    let (ahead, behind) = compute_ahead_behind(&repo);

    // File statuses
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    if sub_cfg.ignore_dirty {
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
            submodule_warn: SubmoduleWarn::default(),
        });
    }

    let is_dirty = !files.is_empty();

    // Collect linked worktree details (excludes the main working tree)
    let worktree_info = collect_worktree_info(&repo);

    // Detect submodules by checking for .gitmodules
    let has_submodules = path.join(".gitmodules").is_file();

    // Submodule enumeration
    let mut submodules = Vec::new();
    let mut has_dirty_submodules = false;
    let mut has_unpushed_submodules = false;

    // Iterate when *any* submodule signal is requested. `ignore_dirty` and
    // `warn_unpushed` are independent: even with dirty hidden, we may still
    // need to surface unpushed-pointer warnings.
    if has_submodules
        && (!sub_cfg.ignore_dirty || sub_cfg.warn_unpushed)
        && let Ok(subs) = repo.submodules()
    {
        for sub in &subs {
            let name = sub.name().unwrap_or("").to_string();
            let sub_path = PathBuf::from(sub.path());

            // Dirty-state mapping (gated on !ignore_dirty).
            let state = if sub_cfg.ignore_dirty {
                None
            } else {
                let status = repo
                    .submodule_status(&name, git2::SubmoduleIgnore::Unspecified)
                    .unwrap_or(SubmoduleStatus::empty());
                if status.is_wd_uninitialized() {
                    Some(SubmoduleState::Uninitialized)
                } else if status.is_wd_wd_modified()
                    || status.contains(SubmoduleStatus::WD_UNTRACKED)
                {
                    Some(SubmoduleState::Dirty)
                } else if status.is_wd_modified()
                    || status.contains(SubmoduleStatus::WD_INDEX_MODIFIED)
                {
                    Some(SubmoduleState::Modified)
                } else {
                    None
                }
            };

            // Warn-state computation (gated on warn_unpushed). Skipped for
            // uninitialized submodules — `sub.open()` would fail anyway.
            let warn = if sub_cfg.warn_unpushed
                && state != Some(SubmoduleState::Uninitialized)
            {
                compute_submodule_warn(sub)
            } else {
                SubmoduleWarn::default()
            };

            let has_dirty_signal = state.is_some();
            let has_warn_signal = !warn.is_clean();

            if !has_dirty_signal && !has_warn_signal {
                continue;
            }

            let head_oid = sub.head_id().map(|id| id.to_string());
            let workdir_oid = sub.workdir_id().map(|id| id.to_string());

            submodules.push(SubmoduleInfo {
                name: name.clone(),
                path: sub_path.clone(),
                state: state.clone(),
                head_oid,
                workdir_oid,
                warn,
            });

            // Cross-reference with files vec
            if let Some(file_entry) = files.iter_mut().find(|f| f.path == sub_path) {
                file_entry.is_submodule = true;
                file_entry.submodule_state = state.clone();
                file_entry.submodule_warn = warn;
            } else {
                // Synthetic FileEntry for any submodule with a dirty or warn signal.
                // FileStatus::Modified keeps the leading `M` "needs attention" cue;
                // the [sub: ...] tag carries the actual semantics.
                files.push(FileEntry {
                    path: sub_path,
                    status: FileStatus::Modified,
                    is_submodule: true,
                    submodule_state: state,
                    submodule_warn: warn,
                });
            }

            if has_dirty_signal {
                has_dirty_submodules = true;
            }
            if has_warn_signal {
                has_unpushed_submodules = true;
            }
        }
    }

    Ok(RepoStatus {
        branch,
        files,
        ahead,
        behind,
        is_dirty: is_dirty || has_dirty_submodules,
        worktree_info,
        has_submodules,
        submodules,
        has_dirty_submodules,
        has_unpushed_submodules,
        fetch_failed,
    })
}

/// Compute "you owe a push" warnings for a submodule.
/// Uses `index_id()` (parent's *staged* pointer, not committed) so the warning
/// fires *before* the parent commit ships, while the user can still amend or reset.
fn compute_submodule_warn(sub: &git2::Submodule) -> SubmoduleWarn {
    let recorded = match sub.index_id() {
        Some(o) => o,
        None => return SubmoduleWarn::default(),
    };
    let inner = match sub.open() {
        Ok(r) => r,
        Err(_) => return SubmoduleWarn::default(),
    };

    let unpushed_commits = compute_ahead_behind(&inner).0;

    // If the recorded oid isn't even in local objects, no remote can possibly hold it.
    if inner.find_object(recorded, None).is_err() {
        return SubmoduleWarn {
            unpushed_commits,
            pointer_unreachable: true,
        };
    }

    if let Ok(branches) = inner.branches(Some(git2::BranchType::Remote)) {
        for (b, _) in branches.flatten() {
            if let Some(tip) = b.get().target()
                && (tip == recorded
                    || inner.graph_descendant_of(tip, recorded).unwrap_or(false))
            {
                return SubmoduleWarn {
                    unpushed_commits,
                    pointer_unreachable: false,
                };
            }
        }
    }

    // No remote tip reaches `recorded` (or no remotes configured) → unreachable.
    SubmoduleWarn {
        unpushed_commits,
        pointer_unreachable: true,
    }
}

/// Collect details for each linked worktree using the git2 API.
/// Mirrors the pattern in `git/graph.rs::collect_worktree_branches`.
fn collect_worktree_info(repo: &Repository) -> Vec<WorktreeEntry> {
    let wt_names = match repo.worktrees() {
        Ok(names) => names,
        Err(_) => return Vec::new(),
    };
    let mut entries = Vec::new();
    for i in 0..wt_names.len() {
        let name = match wt_names.get(i) {
            Some(n) => n,
            None => continue,
        };
        let wt = match repo.find_worktree(name) {
            Ok(wt) => wt,
            Err(_) => continue,
        };
        let wt_path = wt.path().to_path_buf();
        let branch = match Repository::open(&wt_path) {
            Ok(wt_repo) => match wt_repo.head() {
                Ok(head) => head.shorthand().unwrap_or("HEAD").to_string(),
                Err(_) => "(no branch)".to_string(),
            },
            Err(_) => continue,
        };
        entries.push(WorktreeEntry {
            name: name.to_string(),
            path: wt_path,
            branch,
        });
    }
    entries
}

/// Run `git fetch` with a 30-second timeout to update remote-tracking refs.
/// Uses the CLI because git2 fetch doesn't support SSH agent / credential helpers
/// out of the box. Returns `true` on success, `false` on failure/timeout.
fn fetch_remote_silent(path: &Path) -> bool {
    use wait_timeout::ChildExt;

    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("fetch")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match child {
        Ok(mut c) => {
            match c.wait_timeout(std::time::Duration::from_secs(30)) {
                Ok(Some(status)) => status.success(),
                Ok(None) => {
                    // Timed out — kill the hung process
                    let _ = c.kill();
                    let _ = c.wait();
                    false
                }
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
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
    fn test_worktree_info_empty_for_plain_repo() {
        let (tmp, _repo) = init_temp_repo();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.worktree_info.is_empty());
    }

    #[test]
    fn test_worktree_info_reflects_linked_worktrees() {
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

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert_eq!(status.worktree_info.len(), 1);
        assert_eq!(status.worktree_info[0].branch, "wt-branch");
        assert_eq!(status.worktree_info[0].name, "wt1");
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
            is_submodule: true,
            submodule_state: Some(SubmoduleState::Modified),
            submodule_warn: SubmoduleWarn::default(),
        };
        assert!(entry.is_submodule);
        assert_eq!(entry.submodule_state, Some(SubmoduleState::Modified));
        assert!(entry.submodule_warn.is_clean());

        let plain = FileEntry {
            path: PathBuf::from("src/main.rs"),
            status: FileStatus::Modified,
            is_submodule: false,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
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
        let status = query_status(
            tmp.path(),
            &SubmoduleConfig {
                ignore_dirty: true,
                warn_unpushed: false,
            },
        ).unwrap();
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
        ).unwrap();
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
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());

        // Now make the submodule dirty by modifying a file inside it
        let sub_workdir = tmp.path().join("my-sub");
        fs::write(sub_workdir.join("lib.rs"), "fn hello() { /* changed */ }").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        assert_eq!(sub_info.state, Some(SubmoduleState::Dirty));

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
        let status = query_status(
            tmp.path(),
            &SubmoduleConfig {
                ignore_dirty: true,
                warn_unpushed: false,
            },
        ).unwrap();
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
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(status.has_dirty_submodules);
        assert!(!status.submodules.is_empty());

        let sub_info = &status.submodules[0];
        assert_eq!(sub_info.path, Path::new("my-sub"));
        // Could be Modified or Dirty depending on exact git state
        assert!(
            sub_info.state == Some(SubmoduleState::Modified)
                || sub_info.state == Some(SubmoduleState::Dirty),
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
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        assert!(!status.has_dirty_submodules);
        assert!(status.submodules.is_empty());
        assert!(!status.files.iter().any(|f| f.is_submodule));
    }

    #[test]
    fn test_dirty_submodule_makes_repo_dirty() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

        // Start clean
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(!status.is_dirty);

        // Make submodule dirty
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        // Now repo should be dirty
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.is_dirty);
    }

    #[test]
    fn test_submodule_warn_default_is_clean() {
        let warn = SubmoduleWarn::default();
        assert!(warn.is_clean());
        assert_eq!(warn.unpushed_commits, 0);
        assert!(!warn.pointer_unreachable);
    }

    #[test]
    fn test_submodule_warn_is_clean_predicate() {
        assert!(SubmoduleWarn::default().is_clean());
        assert!(
            !SubmoduleWarn {
                unpushed_commits: 1,
                pointer_unreachable: false,
            }
            .is_clean()
        );
        assert!(
            !SubmoduleWarn {
                unpushed_commits: 0,
                pointer_unreachable: true,
            }
            .is_clean()
        );
    }

    #[test]
    fn test_clean_submodule_warn_fields_clean() {
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        // Freshly cloned via `git submodule add` — recorded oid is on origin.
        assert!(!status.has_unpushed_submodules);
    }

    #[test]
    fn test_dirty_submodule_warn_fields_stay_clean() {
        // A workdir-dirty submodule should not trigger warn fields — only
        // pointer changes / unpushed commits matter for "you owe a push".
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        let sub_info = &status.submodules[0];
        assert!(sub_info.warn.is_clean());

        let file_entry = status
            .files
            .iter()
            .find(|f| f.path == Path::new("my-sub"))
            .unwrap();
        assert!(file_entry.submodule_warn.is_clean());
    }

    #[test]
    fn test_warn_unpushed_false_zeros_warn_fields_even_when_dirty() {
        // With warn_unpushed=false, dirty subs still surface but warn fields stay zero.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: false,
            warn_unpushed: false,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        assert!(status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        assert!(status.submodules[0].warn.is_clean());
    }

    #[test]
    fn test_ignore_dirty_with_warn_unpushed_iterates_subs() {
        // When ignore_dirty=true but warn_unpushed=true, the loop must still
        // iterate submodules to compute warn fields. Dirty state itself is hidden.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: true,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        // Dirty signal hidden
        assert!(!status.has_dirty_submodules);
        // Warn signal also clean for this freshly-added submodule
        assert!(!status.has_unpushed_submodules);
        // No submodule entries (nothing to warn about; dirty hidden)
        assert!(status.submodules.is_empty());
    }

    #[test]
    fn test_both_flags_off_skips_submodule_loop_entirely() {
        // When both flags are off, the loop body is skipped — fast path.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

        let cfg = SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: false,
        };
        let status = query_status(tmp.path(), &cfg).unwrap();
        assert!(status.has_submodules); // .gitmodules still exists
        assert!(!status.has_dirty_submodules);
        assert!(!status.has_unpushed_submodules);
        assert!(status.submodules.is_empty());
    }

    #[test]
    fn test_uninitialized_submodule_does_not_panic_on_warn_check() {
        // De-init the submodule so .git is absent — `sub.open()` should fail
        // gracefully and `compute_submodule_warn` returns default.
        let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["submodule", "deinit", "-f", "my-sub"])
            .output()
            .unwrap();

        let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
        assert!(status.has_submodules);
        // No panic. The submodule may or may not appear in `submodules` (the
        // dirty-state check still classifies it as Uninitialized) — the key
        // invariant is that warn fields stay clean.
        for sub in &status.submodules {
            assert!(sub.warn.is_clean());
        }
        assert!(!status.has_unpushed_submodules);
    }
}
