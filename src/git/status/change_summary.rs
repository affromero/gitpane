use crate::config::SubmoduleConfig;
use git2::{Repository, Status, StatusOptions, SubmoduleStatus};
use std::path::{Path, PathBuf};

use super::submodule::{
    diff_submodule_ignore, observe_child, regular_submodule_blocker, submodule_head_and_warn,
    submodule_registrations, working_submodule_modified,
};
use super::{FileEntry, FileStatus, SubmoduleInfo, SubmoduleState, SubmoduleWarn};

pub(super) struct ChangeSummary {
    pub(super) files: Vec<FileEntry>,
    pub(super) file_count_lower_bound: usize,
    pub(super) is_dirty: bool,
    pub(super) has_submodules: bool,
    pub(super) submodules: Vec<SubmoduleInfo>,
    pub(super) has_dirty_submodules: bool,
    pub(super) has_unpushed_submodules: bool,
}

pub(super) fn status_options(
    recurse_untracked_dirs: bool,
    sub_cfg: &SubmoduleConfig,
) -> StatusOptions {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(recurse_untracked_dirs)
        .renames_head_to_index(true);
    if sub_cfg.ignore_dirty {
        options.exclude_submodules(true);
    }
    options
}

pub(super) fn file_entry(path: PathBuf, s: Status) -> Option<FileEntry> {
    let status = if s.is_conflicted() {
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
    } else if s.is_index_modified()
        || s.is_wt_modified()
        || s.is_index_typechange()
        || s.is_wt_typechange()
    {
        FileStatus::Modified
    } else {
        return None;
    };
    Some(FileEntry {
        path,
        status,
        staged: s.is_index_new()
            || s.is_index_modified()
            || s.is_index_deleted()
            || s.is_index_renamed()
            || s.is_index_typechange(),
        unstaged: s.is_wt_new()
            || s.is_wt_modified()
            || s.is_wt_deleted()
            || s.is_wt_renamed()
            || s.is_wt_typechange(),
        is_submodule: false,
        submodule_state: None,
        submodule_warn: SubmoduleWarn::default(),
        submodule_head: None,
    })
}

pub(super) struct SubmoduleObservation {
    pub(super) path: PathBuf,
    pub(super) in_index: bool,
    pub(super) status: SubmoduleStatus,
    pub(super) details: Option<SubmoduleInfo>,
}

pub(super) fn collect_submodule_observations(
    repo: &Repository,
    path: &Path,
    sub_cfg: &SubmoduleConfig,
    strict: bool,
) -> color_eyre::Result<Vec<SubmoduleObservation>> {
    let mut submodules = Vec::new();
    let display_details = path.join(".gitmodules").is_file();
    if !sub_cfg.ignore_dirty || sub_cfg.warn_unpushed {
        let diff_ignore = diff_submodule_ignore(repo)?;
        let subs = match submodule_registrations(repo) {
            Ok(subs) => subs,
            Err(error) if strict => return Err(error.into()),
            Err(_) => Vec::new(),
        };
        for sub in &subs {
            let name = match std::str::from_utf8(&sub.name) {
                Ok(name) => name.to_string(),
                Err(error) if strict => return Err(error.into()),
                Err(_) => String::new(),
            };
            let sub_path = sub.path.clone();
            if !sub_path
                .components()
                .any(|part| matches!(part, std::path::Component::Normal(_)))
                || sub_path.components().any(|part| {
                    !matches!(
                        part,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                })
            {
                return Err(color_eyre::eyre::eyre!(
                    "Submodule path must remain inside its parent"
                ));
            }
            let child_path = path.join(&sub_path);
            let replaced_link = child_path
                .symlink_metadata()
                .is_ok_and(|entry| entry.file_type().is_symlink());

            // Dirty-state mapping (gated on !ignore_dirty).
            let uninitialized = if replaced_link {
                true
            } else if strict {
                match path.join(&sub_path).join(".git").symlink_metadata() {
                    Ok(_) => false,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
                        if regular_submodule_blocker(&path.join(&sub_path))?.is_some() {
                            true
                        } else {
                            return Err(error.into());
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                false
            };
            let ignore = sub.ignore;
            let working_inspection = !sub_cfg.ignore_dirty
                && !matches!(
                    diff_ignore,
                    git2::SubmoduleIgnore::Unspecified | git2::SubmoduleIgnore::All
                );
            let inner = if uninitialized
                || (ignore == git2::SubmoduleIgnore::All
                    && !sub_cfg.warn_unpushed
                    && !working_inspection)
            {
                None
            } else {
                let open = || -> color_eyre::Result<Repository> {
                    child_path.join(".git").symlink_metadata()?;
                    let inner = Repository::open(&child_path)?;
                    let workdir = inner
                        .workdir()
                        .ok_or_else(|| color_eyre::eyre::eyre!("Submodule has no working tree"))?;
                    if workdir.canonicalize()? != child_path.canonicalize()? {
                        return Err(color_eyre::eyre::eyre!(
                            "Submodule path does not identify its own repository"
                        ));
                    }
                    Ok(inner)
                };
                match open() {
                    Ok(inner) => Some(inner),
                    Err(error) if strict => return Err(error),
                    Err(_) => None,
                }
            };
            let inspect = if sub_cfg.ignore_dirty && !strict {
                git2::SubmoduleIgnore::Dirty
            } else {
                ignore
            };
            let observation = inner
                .as_ref()
                .map(|inner| observe_child(inner, sub.index_oid, inspect))
                .transpose()?;
            let status = if !sub_cfg.ignore_dirty || (strict && sub_cfg.warn_unpushed) {
                if replaced_link && ignore == git2::SubmoduleIgnore::All {
                    Some(SubmoduleStatus::empty())
                } else if uninitialized {
                    Some(SubmoduleStatus::WD_UNINITIALIZED)
                } else if let Some(observation) = &observation {
                    Some(observation.status)
                } else if ignore == git2::SubmoduleIgnore::All {
                    Some(SubmoduleStatus::empty())
                } else if path.join(&sub_path).is_dir()
                    && !path.join(&sub_path).join(".git").exists()
                {
                    Some(SubmoduleStatus::WD_UNINITIALIZED)
                } else {
                    Some(SubmoduleStatus::WD_DELETED)
                }
            } else {
                None
            };
            let state = if sub_cfg.ignore_dirty {
                None
            } else {
                let status = status.unwrap_or(SubmoduleStatus::empty());
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

            // Read branch and warning details only for initialized checkouts
            // with a dirty or warning signal to display.
            let is_uninit = state == Some(SubmoduleState::Uninitialized)
                || status.is_some_and(|status| status.is_wd_uninitialized());
            let (head, warn) = if !is_uninit
                && (state.is_some() || sub_cfg.warn_unpushed)
                && let Some(inner) = &inner
            {
                submodule_head_and_warn(inner, sub.index_oid, sub_cfg.warn_unpushed)
            } else {
                (None, SubmoduleWarn::default())
            };

            let has_dirty_signal = state.is_some();
            let has_warn_signal = !warn.is_clean();

            let head_oid = sub.head_oid.map(|id| id.to_string());
            let workdir_oid = observation
                .as_ref()
                .and_then(|observation| observation.head_oid)
                .map(|id| id.to_string());

            let working_status = if !sub_cfg.ignore_dirty
                && diff_ignore != git2::SubmoduleIgnore::Unspecified
                && diff_ignore != inspect
            {
                inner
                    .as_ref()
                    .map(|inner| observe_child(inner, sub.index_oid, diff_ignore))
                    .transpose()?
                    .map(|observed| observed.status)
                    .unwrap_or(SubmoduleStatus::empty())
            } else {
                status.unwrap_or(SubmoduleStatus::empty())
            };

            submodules.push(SubmoduleObservation {
                path: sub_path.clone(),
                in_index: sub.index_oid.is_some(),
                status: working_status,
                details: (display_details && (has_dirty_signal || has_warn_signal)).then_some(
                    SubmoduleInfo {
                        name,
                        path: sub_path,
                        state,
                        head,
                        head_oid,
                        workdir_oid,
                        warn,
                    },
                ),
            });
        }
    }
    Ok(submodules)
}

pub(super) fn working_diff<'repo>(
    repo: &'repo Repository,
    index: &git2::Index,
    recurse: bool,
) -> Result<git2::Diff<'repo>, git2::Error> {
    let mut options = git2::DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(recurse)
        .include_typechange(true)
        .ignore_submodules(true);
    repo.diff_index_to_workdir(Some(index), Some(&mut options))
}

pub(super) fn working_status(delta: &git2::DiffDelta<'_>) -> Status {
    match delta.status() {
        git2::Delta::Added | git2::Delta::Untracked => Status::WT_NEW,
        git2::Delta::Deleted => Status::WT_DELETED,
        git2::Delta::Modified => Status::WT_MODIFIED,
        git2::Delta::Renamed => Status::WT_RENAMED,
        git2::Delta::Typechange => Status::WT_TYPECHANGE,
        git2::Delta::Conflicted => Status::CONFLICTED,
        _ => Status::empty(),
    }
}

pub(super) fn collect_change_summary(
    repo: &Repository,
    path: &Path,
    recurse_untracked_dirs: bool,
    sub_cfg: &SubmoduleConfig,
    strict: bool,
) -> color_eyre::Result<ChangeSummary> {
    if repo.is_bare() {
        return Ok(ChangeSummary {
            files: Vec::new(),
            file_count_lower_bound: 0,
            is_dirty: false,
            has_submodules: false,
            submodules: Vec::new(),
            has_dirty_submodules: false,
            has_unpushed_submodules: false,
        });
    }
    let mut index = repo.index()?;
    index.read(false)?;
    let mut opts = status_options(recurse_untracked_dirs, sub_cfg);
    opts.no_refresh(true);
    if !sub_cfg.ignore_dirty {
        opts.show(git2::StatusShow::Index);
    }
    // Preserve Git's distinct `child` staged deletion and `child/`
    // untracked-directory entries. Path equality normalizes that separator.
    let mut flags = std::collections::BTreeMap::<std::ffi::OsString, Status>::new();
    for entry in repo.statuses(Some(&mut opts))?.iter() {
        let delta = entry
            .index_to_workdir()
            .or_else(|| entry.head_to_index())
            .ok_or_else(|| color_eyre::eyre::eyre!("Git status entry has no delta"))?;
        let path = delta
            .new_file()
            .path()
            .map(|path| path.as_os_str().to_owned())
            .ok_or_else(|| color_eyre::eyre::eyre!("Git status entry has no path"))?;
        *flags.entry(path).or_insert(Status::empty()) |= entry.status();
    }
    if !sub_cfg.ignore_dirty {
        for delta in working_diff(repo, &index, recurse_untracked_dirs)?.deltas() {
            let path = delta
                .new_file()
                .path()
                .map(|path| path.as_os_str().to_owned())
                .ok_or_else(|| color_eyre::eyre::eyre!("Git working delta has no path"))?;
            *flags.entry(path).or_insert(Status::empty()) |= working_status(&delta);
        }
    }
    let observations = collect_submodule_observations(repo, path, sub_cfg, strict)?;
    if !sub_cfg.ignore_dirty {
        for module in &observations {
            if module.in_index && working_submodule_modified(module.status) {
                *flags
                    .entry(module.path.as_os_str().to_owned())
                    .or_insert(Status::empty()) |= Status::WT_MODIFIED;
            }
        }
    }
    let mut files: Vec<_> = flags
        .into_iter()
        .filter_map(|(path, flags)| file_entry(path.into(), flags))
        .collect();

    let is_dirty = !files.is_empty();
    let mut file_count_lower_bound = files.len();

    // Detect submodules by checking for .gitmodules
    let has_submodules = path.join(".gitmodules").is_file();

    let submodules: Vec<_> = observations
        .into_iter()
        .filter_map(|module| module.details)
        .collect();
    let mut has_dirty_submodules = false;
    let mut has_unpushed_submodules = false;
    for module in &submodules {
        let has_dirty_signal = module.state.is_some();
        has_dirty_submodules |= has_dirty_signal;
        has_unpushed_submodules |= !module.warn.is_clean();

        if let Some(file_entry) = files.iter_mut().find(|file| file.path == module.path) {
            file_entry.is_submodule = true;
            file_entry.submodule_state = module.state.clone();
            file_entry.submodule_warn = module.warn;
            file_entry.submodule_head = module.head.clone();
        } else {
            files.push(FileEntry {
                path: module.path.clone(),
                status: FileStatus::Modified,
                staged: false,
                unstaged: true,
                is_submodule: true,
                submodule_state: module.state.clone(),
                submodule_warn: module.warn,
                submodule_head: module.head.clone(),
            });
            if has_dirty_signal {
                file_count_lower_bound += 1;
            }
        }
    }

    Ok(ChangeSummary {
        files,
        file_count_lower_bound,
        is_dirty: is_dirty || has_dirty_submodules,
        has_submodules,
        submodules,
        has_dirty_submodules,
        has_unpushed_submodules,
    })
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
