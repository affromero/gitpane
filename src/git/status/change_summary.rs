use crate::config::SubmoduleConfig;
use git2::{Repository, StatusOptions, SubmoduleStatus};
use std::path::{Path, PathBuf};

use super::submodule::compute_submodule_head_and_warn;
use super::{FileEntry, FileStatus, SubmoduleInfo, SubmoduleState, SubmoduleWarn};

pub(super) struct ChangeSummary {
    pub(super) files: Vec<FileEntry>,
    pub(super) is_dirty: bool,
    pub(super) has_submodules: bool,
    pub(super) submodules: Vec<SubmoduleInfo>,
    pub(super) has_dirty_submodules: bool,
    pub(super) has_unpushed_submodules: bool,
}

pub(super) fn collect_change_summary(
    repo: &Repository,
    path: &Path,
    recurse_untracked_dirs: bool,
    sub_cfg: &SubmoduleConfig,
) -> color_eyre::Result<ChangeSummary> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(recurse_untracked_dirs)
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
        });
    }

    let is_dirty = !files.is_empty();

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

            // Open the submodule (once) to read its checked-out branch and,
            // when enabled, compute push/merge warnings. Skip for uninitialized
            // submodules (`sub.open()` fails) and when there is nothing to show
            // (clean working tree with warnings disabled): branch is only worth
            // reading for a submodule that will render a row.
            let is_uninit = state == Some(SubmoduleState::Uninitialized);
            let (head, warn) = if !is_uninit && (state.is_some() || sub_cfg.warn_unpushed) {
                compute_submodule_head_and_warn(sub, sub_cfg.warn_unpushed)
            } else {
                (None, SubmoduleWarn::default())
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
                head: head.clone(),
                head_oid,
                workdir_oid,
                warn,
            });

            // Cross-reference with files vec
            if let Some(file_entry) = files.iter_mut().find(|f| f.path == sub_path) {
                file_entry.is_submodule = true;
                file_entry.submodule_state = state.clone();
                file_entry.submodule_warn = warn;
                file_entry.submodule_head = head;
            } else {
                // Synthetic FileEntry for any submodule with a dirty or warn signal.
                // FileStatus::Modified keeps the leading `M` "needs attention" cue;
                // the [sub: ...] tag carries the actual semantics.
                files.push(FileEntry {
                    path: sub_path,
                    status: FileStatus::Modified,
                    // A dirty submodule is a worktree-side signal; staging is
                    // gated off for submodule rows anyway.
                    staged: false,
                    unstaged: true,
                    is_submodule: true,
                    submodule_state: state,
                    submodule_warn: warn,
                    submodule_head: head,
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

    Ok(ChangeSummary {
        files,
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
