use git2::{Repository, StatusOptions};
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
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    pub status: FileStatus,
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
pub(crate) fn query_status(path: &Path) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, false)
}

/// Status query with `git fetch` first. Used by explicit user refresh (`r` key).
pub(crate) fn query_status_with_fetch(path: &Path) -> color_eyre::Result<RepoStatus> {
    query_status_inner(path, true)
}

fn query_status_inner(path: &Path, fetch: bool) -> color_eyre::Result<RepoStatus> {
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
        });
    }

    let is_dirty = !files.is_empty();

    // Count linked worktrees (excludes the main working tree)
    let worktrees = repo.worktrees().map(|wt| wt.len()).unwrap_or(0);

    Ok(RepoStatus {
        branch,
        files,
        ahead,
        behind,
        is_dirty,
        worktrees,
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
        let status = query_status(tmp.path()).unwrap();
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

        let status = query_status(tmp.path()).unwrap();
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

        let status = query_status(tmp.path()).unwrap();
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
        let status = query_status(tmp.path()).unwrap();
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

        let status = query_status(tmp.path()).unwrap();
        assert_eq!(status.worktrees, 1);
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
}
