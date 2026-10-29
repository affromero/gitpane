use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use git2::{Delta, FileMode, Index, IndexEntry, IndexEntryExtendedFlag, Repository, Status};
use serde::Serialize;

use crate::config::SubmoduleConfig;

use super::change_summary::{collect_change_summary, status_options};

#[derive(Debug, Serialize)]
pub(crate) struct ConflictStages {
    base: Option<String>,
    ours: Option<String>,
    theirs: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChangeRecord {
    path: String,
    group: &'static str,
    kind: &'static str,
    original_path: Option<String>,
    conflict_stages: Option<ConflictStages>,
    is_submodule: bool,
}

#[derive(Debug)]
pub(crate) enum ChangeError {
    Git(git2::Error),
    Repository(String),
    InvalidPath,
    Limit,
}

impl From<git2::Error> for ChangeError {
    fn from(error: git2::Error) -> Self {
        Self::Git(error)
    }
}

fn path_string(path: &Path) -> Result<String, ChangeError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ChangeError::InvalidPath);
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or(ChangeError::InvalidPath)
}

fn index_path(entry: &IndexEntry) -> Result<String, ChangeError> {
    let path = std::str::from_utf8(&entry.path).map_err(|_| ChangeError::InvalidPath)?;
    path_string(Path::new(path))
}

fn selected_path(delta: &git2::DiffDelta<'_>, deleted: bool) -> Result<String, ChangeError> {
    let file = if deleted {
        delta.old_file()
    } else {
        delta.new_file()
    };
    let path = file.path().ok_or(ChangeError::InvalidPath)?;
    path_string(path)
}

fn add(
    records: &mut Vec<ChangeRecord>,
    max_records: usize,
    record: ChangeRecord,
) -> Result<(), ChangeError> {
    if records.len() >= max_records {
        return Err(ChangeError::Limit);
    }
    records.push(record);
    Ok(())
}

fn collect_conflicts(
    index: &Index,
    records: &mut Vec<ChangeRecord>,
    max_records: usize,
    submodules: &HashSet<PathBuf>,
) -> Result<(), ChangeError> {
    if !index.has_conflicts() {
        return Ok(());
    }
    for conflict in index.conflicts()? {
        let conflict = conflict?;
        let base = conflict.ancestor.as_ref().map(index_path).transpose()?;
        let ours = conflict.our.as_ref().map(index_path).transpose()?;
        let theirs = conflict.their.as_ref().map(index_path).transpose()?;
        let path = ours
            .as_ref()
            .or(theirs.as_ref())
            .or(base.as_ref())
            .ok_or(ChangeError::InvalidPath)?
            .clone();
        let is_submodule = [&base, &ours, &theirs]
            .into_iter()
            .flatten()
            .any(|value| submodules.contains(Path::new(value)))
            || [
                conflict.ancestor.as_ref(),
                conflict.our.as_ref(),
                conflict.their.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|entry| entry.mode & 0o170000 == 0o160000);
        add(
            records,
            max_records,
            ChangeRecord {
                path,
                group: "merge",
                kind: "conflicted",
                original_path: None,
                conflict_stages: Some(ConflictStages { base, ours, theirs }),
                is_submodule,
            },
        )?;
    }
    Ok(())
}

fn intent_to_add(index: &Index, path: &str) -> bool {
    index.get_path(Path::new(path), 0).is_some_and(|entry| {
        IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended).is_intent_to_add()
    })
}

fn delta_is_submodule(
    delta: &git2::DiffDelta<'_>,
    index: &Index,
    path: &str,
    submodules: &HashSet<PathBuf>,
) -> bool {
    submodules.contains(Path::new(path))
        || delta.old_file().mode() == FileMode::Commit
        || delta.new_file().mode() == FileMode::Commit
        || index
            .get_path(Path::new(path), 0)
            .is_some_and(|entry| entry.mode & 0o170000 == 0o160000)
}

fn add_index(
    status: Status,
    delta: Option<git2::DiffDelta<'_>>,
    index: &Index,
    records: &mut Vec<ChangeRecord>,
    max_records: usize,
    submodules: &HashSet<PathBuf>,
) -> Result<(), ChangeError> {
    let kind = if status.is_index_new() {
        "index_added"
    } else if status.is_index_deleted() {
        "index_deleted"
    } else if status.is_index_renamed() {
        "index_renamed"
    } else if status.is_index_typechange() {
        "index_type_changed"
    } else if status.is_index_modified() {
        "index_modified"
    } else {
        return Ok(());
    };
    let delta = delta.ok_or(ChangeError::InvalidPath)?;
    let path = selected_path(&delta, status.is_index_deleted())?;
    if status.is_index_new() && intent_to_add(index, &path) {
        return Ok(());
    }
    let original = if matches!(delta.status(), Delta::Renamed | Delta::Copied) {
        Some(selected_path(&delta, true)?)
    } else {
        None
    };
    let kind = if delta.status() == Delta::Copied {
        "index_copied"
    } else {
        kind
    };
    let is_submodule = delta_is_submodule(&delta, index, &path, submodules);
    add(
        records,
        max_records,
        ChangeRecord {
            path,
            group: "index",
            kind,
            original_path: original,
            conflict_stages: None,
            is_submodule,
        },
    )
}

fn add_working(
    status: Status,
    delta: Option<git2::DiffDelta<'_>>,
    index: &Index,
    records: &mut Vec<ChangeRecord>,
    max_records: usize,
    submodules: &HashSet<PathBuf>,
) -> Result<(), ChangeError> {
    if !(status.is_wt_new()
        || status.is_wt_deleted()
        || status.is_wt_renamed()
        || status.is_wt_typechange()
        || status.is_wt_modified())
    {
        return Ok(());
    }
    let delta = delta.ok_or(ChangeError::InvalidPath)?;
    let path = selected_path(&delta, status.is_wt_deleted())?;
    // libgit2 does not recurse into an untracked directory containing a
    // `.git` entry. It reports that directory with a trailing slash, even
    // when recursive untracked-file enumeration is enabled.
    let directory = status.is_wt_new() && path.ends_with('/');
    let path = if directory {
        path.trim_end_matches('/').to_owned()
    } else {
        path
    };
    let kind = if status.is_wt_deleted() {
        "working_deleted"
    } else if intent_to_add(index, &path) {
        "intent_to_add"
    } else if directory {
        "untracked_directory"
    } else if status.is_wt_new() {
        "untracked"
    } else if status.is_wt_renamed() {
        "working_renamed"
    } else if status.is_wt_typechange() {
        "type_changed"
    } else {
        "working_modified"
    };
    let original = if kind == "working_renamed" {
        Some(selected_path(&delta, true)?)
    } else {
        None
    };
    let group = if matches!(kind, "untracked" | "untracked_directory") {
        "untracked"
    } else {
        "working"
    };
    let is_submodule = delta_is_submodule(&delta, index, &path, submodules);
    add(
        records,
        max_records,
        ChangeRecord {
            path,
            group,
            kind,
            original_path: original,
            conflict_stages: None,
            is_submodule,
        },
    )
}

/// Collect independent index and worktree changes for consumers that need
/// individual files. The TUI continues using its compact summary.
pub(crate) fn collect_changes(
    repo: &Repository,
    sub_cfg: &SubmoduleConfig,
    max_records: usize,
) -> Result<Vec<ChangeRecord>, ChangeError> {
    let index = repo.index()?;
    let submodules: HashSet<_> = repo
        .submodules()?
        .into_iter()
        .map(|module| module.path().to_owned())
        .collect();
    let mut records = Vec::new();
    collect_conflicts(&index, &mut records, max_records, &submodules)?;
    // Keep worktree renames as the deletion and untracked addition Git's
    // file actions operate on. Staged rename detection remains enabled.
    let mut options = status_options(true, sub_cfg);
    let statuses = repo.statuses(Some(&mut options))?;
    for entry in statuses.iter() {
        let status = entry.status();
        if status.is_conflicted() {
            continue;
        }
        add_index(
            status,
            entry.head_to_index(),
            &index,
            &mut records,
            max_records,
            &submodules,
        )?;
        add_working(
            status,
            entry.index_to_workdir(),
            &index,
            &mut records,
            max_records,
            &submodules,
        )?;
    }
    if sub_cfg.warn_unpushed && !submodules.is_empty() {
        let workdir = repo.workdir().ok_or_else(|| {
            ChangeError::Repository("Repository has no working directory.".to_owned())
        })?;
        let summary = collect_change_summary(repo, workdir, false, sub_cfg, true)
            .map_err(|error| ChangeError::Repository(error.to_string()))?;
        let changed_paths: HashSet<_> = records.iter().map(|record| record.path.clone()).collect();
        for module in summary.submodules {
            if module.warn.is_clean() {
                continue;
            }
            let path = path_string(&module.path)?;
            if changed_paths.contains(&path) {
                continue;
            }
            add(
                &mut records,
                max_records,
                ChangeRecord {
                    path,
                    group: "attention",
                    kind: "submodule_attention",
                    original_path: None,
                    conflict_stages: None,
                    is_submodule: true,
                },
            )?;
        }
    }
    records.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.group.cmp(b.group))
            .then(a.kind.cmp(b.kind))
    });
    Ok(records)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use git2::{FileMode, IndexEntry, IndexTime, Repository, Signature};
    use tempfile::TempDir;

    use crate::config::SubmoduleConfig;
    use crate::git::status::change_summary::status_options;

    use super::delta_is_submodule;

    #[test]
    fn deleted_gitlink_is_identified_without_a_configured_module_name() {
        let temporary = TempDir::new().unwrap();
        let repo = Repository::init(temporary.path()).unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now("Test", "test@example.invalid").unwrap();
        let root_id = repo
            .commit(Some("HEAD"), &signature, &signature, "root", &tree, &[])
            .unwrap();
        let root = repo.find_commit(root_id).unwrap();
        index
            .add(&IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o160000,
                uid: 0,
                gid: 0,
                file_size: 0,
                id: root_id,
                flags: 0,
                flags_extended: 0,
                path: b"child".to_vec(),
            })
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "gitlink",
            &tree,
            &[&root],
        )
        .unwrap();
        index.remove_path(Path::new("child")).unwrap();
        index.write().unwrap();
        assert!(index.get_path(Path::new("child"), 0).is_none());

        let mut options = status_options(true, &SubmoduleConfig::default());
        let statuses = repo.statuses(Some(&mut options)).unwrap();
        let entry = statuses
            .iter()
            .find(|entry| entry.status().is_index_deleted())
            .unwrap();
        let delta = entry.head_to_index().unwrap();
        assert_eq!(delta.old_file().mode(), FileMode::Commit);
        assert!(delta_is_submodule(&delta, &index, "child", &HashSet::new()));
    }
}
