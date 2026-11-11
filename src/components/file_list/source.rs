use std::path::PathBuf;

use crate::git::stash::{StashFile, StashSource};
use crate::git::status::{FileEntry, FileStatus, SubmoduleWarn};
use crate::repo_id::RepoId;

/// What the changes panel is currently showing. A stash is identified by its
/// immutable OID, never by its list index, so a renumbered stash cannot move
/// the preview onto another snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileSource {
    WorkingTree { repo_id: RepoId, name: String },
    Stash(StashView),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StashView {
    pub repo_id: RepoId,
    pub repo_name: String,
    pub oid: String,
    pub label: String,
    pub summary: String,
}

impl FileSource {
    pub(crate) fn repo_id(&self) -> &RepoId {
        match self {
            Self::WorkingTree { repo_id, .. } => repo_id,
            Self::Stash(stash) => &stash.repo_id,
        }
    }
}

impl StashView {
    /// `repo · stash@{n} · summary`, truncated so the panel title stays readable.
    pub(crate) fn title(&self) -> String {
        let summary = summary_line(&self.summary, 48);
        if summary.is_empty() {
            format!("{} · {}", self.repo_name, self.label)
        } else {
            format!("{} · {} · {}", self.repo_name, self.label, summary)
        }
    }
}

fn summary_line(message: &str, limit: usize) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    if line.chars().count() <= limit {
        return line.to_string();
    }
    let mut truncated: String = line.chars().take(limit.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// One row of a stash preview, mapped from the shared `StashFile` contract.
/// `path` is the file key: the new path, or the old path for a deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StashFileEntry {
    pub source: StashSource,
    pub status: FileStatus,
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub is_binary: bool,
    pub is_submodule: bool,
}

impl StashFileEntry {
    pub(crate) fn from_stash_file(file: StashFile) -> Self {
        Self {
            source: file.source,
            status: stash_status(&file.status),
            path: PathBuf::from(file.path),
            old_path: file.old_path.map(PathBuf::from),
            new_path: file.new_path.map(PathBuf::from),
            is_binary: file.is_binary,
            is_submodule: file.is_submodule,
        }
    }

    /// `old → new` for renames, otherwise the key path.
    pub(crate) fn display_path(&self) -> String {
        match (&self.old_path, &self.new_path) {
            (Some(old), Some(new)) if old != new => {
                format!("{} → {}", old.to_string_lossy(), new.to_string_lossy())
            }
            _ => self.path.to_string_lossy().into_owned(),
        }
    }

    /// Project onto the shared row shape used by the file list. Stash rows do
    /// not carry workspace staged/unstaged flags: their write actions are
    /// refused at dispatch time.
    pub(crate) fn as_file_entry(&self) -> FileEntry {
        FileEntry {
            path: self.path.clone(),
            status: self.status.clone(),
            staged: false,
            unstaged: false,
            is_submodule: self.is_submodule,
            submodule_state: None,
            submodule_warn: SubmoduleWarn::default(),
            submodule_head: None,
        }
    }
}

fn stash_status(status: &str) -> FileStatus {
    match status.chars().next() {
        Some('A') => FileStatus::Added,
        Some('D') => FileStatus::Deleted,
        Some('R') => FileStatus::Renamed,
        Some('U') => FileStatus::Conflicted,
        _ => FileStatus::Modified,
    }
}

/// Loading/feedback state for the changes panel. Kept separate from `files` so
/// an empty stash and a failed read do not both look like "No changes".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ContentState {
    #[default]
    Ready,
    Loading,
    Error(String),
}
