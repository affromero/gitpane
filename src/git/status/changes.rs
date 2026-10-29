use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use git2::{Delta, FileMode, Index, IndexEntry, IndexEntryExtendedFlag, Repository, Status};
use serde::Serialize;

use crate::config::SubmoduleConfig;

use super::change_summary::{
    collect_submodule_observations, status_options, working_diff, working_status,
};
use super::submodule::working_submodule_modified;
use super::{SubmoduleHead, SubmoduleInfo, SubmoduleState, SubmoduleWarn};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    submodule: Option<SubmoduleDetails>,
}

#[derive(Clone, Debug, Serialize)]
struct SubmoduleDetails {
    state: Option<&'static str>,
    head: Option<SubmoduleHeadRecord>,
    warn: SubmoduleWarn,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum SubmoduleHeadRecord {
    Branch { branch: String },
    Detached { detached: bool },
}

impl From<&SubmoduleInfo> for SubmoduleDetails {
    fn from(module: &SubmoduleInfo) -> Self {
        let state = module.state.as_ref().map(|state| match state {
            SubmoduleState::Modified => "modified",
            SubmoduleState::Uninitialized => "uninitialized",
            SubmoduleState::Dirty => "dirty",
        });
        let head = module.head.as_ref().map(|head| match head {
            SubmoduleHead::Branch(branch) => SubmoduleHeadRecord::Branch {
                branch: branch.clone(),
            },
            SubmoduleHead::Detached => SubmoduleHeadRecord::Detached { detached: true },
        });
        Self {
            state,
            head,
            warn: module.warn,
        }
    }
}

impl ChangeRecord {
    pub(crate) fn matches(&self, path: &str, group: &str) -> bool {
        self.path == path && self.group == group
    }
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
                submodule: None,
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
            submodule: None,
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
            submodule: None,
        },
    )
}

/// Read a status pass without refreshing the captured in-memory index.
fn captured_statuses<'repo>(
    repo: &'repo Repository,
    sub_cfg: &SubmoduleConfig,
    show: git2::StatusShow,
) -> Result<git2::Statuses<'repo>, git2::Error> {
    let mut options = status_options(true, sub_cfg);
    options.show(show).no_refresh(true);
    repo.statuses(Some(&mut options))
}

fn current_head(repo: &Repository) -> Result<Option<git2::Oid>, ChangeError> {
    match repo.head().and_then(|head| head.peel_to_commit()) {
        Ok(commit) => Ok(Some(commit.id())),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn require_head(repo: &Repository, expected: Option<git2::Oid>) -> Result<(), ChangeError> {
    if current_head(repo)? != expected {
        return Err(ChangeError::Repository(
            "Repository HEAD changed while collecting changes.".into(),
        ));
    }
    Ok(())
}

/// Collect independent index and worktree changes for consumers that need
/// individual files. The TUI continues using its compact summary.
pub(crate) fn collect_changes(
    repo: &Repository,
    sub_cfg: &SubmoduleConfig,
    max_records: usize,
) -> Result<Vec<ChangeRecord>, ChangeError> {
    let mut index = repo.index()?;
    index.read(false)?;
    let baseline = current_head(repo)?;
    let submodules: HashSet<_> = super::submodule::submodule_registrations(repo)?
        .into_iter()
        .map(|module| module.path)
        .collect();
    let mut records = Vec::new();
    collect_conflicts(&index, &mut records, max_records, &submodules)?;
    // Staged gitlinks belong to the HEAD-to-index comparison independently
    // of checkout state.
    if !sub_cfg.ignore_dirty {
        let staged = captured_statuses(repo, sub_cfg, git2::StatusShow::Index)?;
        for entry in staged.iter() {
            if !entry.status().is_conflicted() {
                add_index(
                    entry.status(),
                    entry.head_to_index(),
                    &index,
                    &mut records,
                    max_records,
                    &submodules,
                )?;
            }
        }
    }
    // Keep worktree renames as the deletion and untracked addition Git's
    // file actions operate on. Staged rename detection remains enabled.
    require_head(repo, baseline)?;
    if sub_cfg.ignore_dirty {
        let statuses = captured_statuses(repo, sub_cfg, git2::StatusShow::IndexAndWorkdir)?;
        for entry in statuses.iter() {
            let status = entry.status();
            if status.is_conflicted() {
                continue;
            }
            // Ignore rules apply to the paired deltas. A non-gitlink type change
            // can retain the staged side even when a pure gitlink is hidden.
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
    } else {
        for delta in working_diff(repo, &index, true)?.deltas() {
            let status = working_status(&delta);
            if !status.is_conflicted() {
                add_working(
                    status,
                    Some(delta),
                    &index,
                    &mut records,
                    max_records,
                    &submodules,
                )?;
            }
        }
    }
    if !submodules.is_empty() {
        let workdir = repo.workdir().ok_or_else(|| {
            ChangeError::Repository("Repository has no working directory.".to_owned())
        })?;
        let modules = collect_submodule_observations(repo, workdir, sub_cfg, true)
            .map_err(|error| ChangeError::Repository(error.to_string()))?;
        let mut working_paths: HashSet<_> = records
            .iter()
            .filter(|record| matches!(record.group, "working" | "untracked" | "merge"))
            .map(|record| record.path.clone())
            .collect();
        let mut details = HashMap::with_capacity(modules.len());
        for module in &modules {
            let path = path_string(&module.path)?;
            if !sub_cfg.ignore_dirty
                && module.in_index
                && working_submodule_modified(module.status)
                && working_paths.insert(path.clone())
            {
                add(
                    &mut records,
                    max_records,
                    ChangeRecord {
                        path: path.clone(),
                        group: "working",
                        kind: "working_modified",
                        original_path: None,
                        conflict_stages: None,
                        is_submodule: true,
                        submodule: None,
                    },
                )?;
            }
            if let Some(info) = &module.details {
                details.insert(path, SubmoduleDetails::from(info));
            }
        }
        let mut changed_paths = HashSet::new();
        for record in &mut records {
            if record.is_submodule {
                record.submodule = details.get(&record.path).cloned();
                changed_paths.insert(record.path.clone());
            }
        }
        for (path, submodule) in details {
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
                    submodule: Some(submodule),
                },
            )?;
        }
    }
    require_head(repo, baseline)?;
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
    fn status_passes_keep_the_captured_index_when_another_process_stages() {
        use crate::git::status::test_support::{commit_index, init_temp_repo};
        let (temporary, repo) = init_temp_repo();
        let path = std::path::Path::new("file");
        std::fs::write(temporary.path().join(path), "original\n").unwrap();
        let mut captured = repo.index().unwrap();
        captured.add_path(path).unwrap();
        captured.write().unwrap();
        commit_index(&repo, "Original");
        let original = captured.get_path(path, 0).unwrap().id;
        let config = SubmoduleConfig::default();
        assert!(
            super::captured_statuses(&repo, &config, git2::StatusShow::Index)
                .unwrap()
                .is_empty()
        );
        std::fs::write(temporary.path().join(path), "new staged content\n").unwrap();
        let external = Repository::open(temporary.path()).unwrap();
        let mut external_index = external.index().unwrap();
        external_index.add_path(path).unwrap();
        external_index.write().unwrap();
        assert_ne!(external_index.get_path(path, 0).unwrap().id, original);
        let working = super::captured_statuses(&repo, &config, git2::StatusShow::Workdir).unwrap();
        assert_eq!(working.len(), 1);
        assert!(working.get(0).unwrap().status().is_wt_modified());
        assert_eq!(captured.get_path(path, 0).unwrap().id, original);
    }

    #[test]
    fn a_changed_head_is_rejected_even_when_the_tree_is_unchanged() {
        use crate::git::status::test_support::{commit_index, init_temp_repo};
        let (_temporary, repo) = init_temp_repo();
        let original = super::current_head(&repo).unwrap();
        let tree = repo.head().unwrap().peel_to_tree().unwrap().id();
        commit_index(&repo, "New commit with unchanged tree");
        assert_eq!(repo.head().unwrap().peel_to_tree().unwrap().id(), tree);
        assert!(super::require_head(&repo, original).is_err());
    }

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
