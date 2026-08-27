use super::render::indicator_columns;
use super::*;
use crate::components::Component;
use crate::git::status::{RepoStatus, StashEntry, WorktreeEntry};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

pub(super) fn empty_status(branch: &str) -> RepoStatus {
    RepoStatus {
        branch: branch.to_string(),
        head_oid: None,
        files: Vec::new(),
        ahead: 0,
        behind: 0,
        is_dirty: false,
        worktree_info: Vec::new(),
        has_submodules: false,
        submodules: Vec::new(),
        has_dirty_submodules: false,
        has_unpushed_submodules: false,
        fetch_failed: false,
        stashes: Vec::new(),
        refs: crate::git::status::RefsFingerprint::default(),
    }
}

pub(super) fn stash_entry(index: usize) -> StashEntry {
    StashEntry {
        index,
        message: format!("WIP {index}"),
        oid: format!("{index:040x}"),
    }
}

pub(super) fn worktree_entry(name: &str) -> WorktreeEntry {
    WorktreeEntry {
        name: name.to_string(),
        path: PathBuf::from(format!("/wt/{name}")),
        branch: name.to_string(),
        ahead: 0,
        behind: 0,
        is_dirty: false,
        file_count: 0,
        has_dirty_submodules: false,
        has_unpushed_submodules: false,
    }
}

pub(super) fn submodule_info(path: PathBuf) -> crate::git::status::SubmoduleInfo {
    crate::git::status::SubmoduleInfo {
        name: path.display().to_string(),
        path,
        state: None,
        head: None,
        head_oid: None,
        workdir_oid: None,
        warn: crate::git::status::SubmoduleWarn::default(),
    }
}

pub(super) fn make_list(paths: &[&str]) -> RepoList {
    make_list_with_expand(paths, true)
}

pub(super) fn make_list_with_expand(paths: &[&str], expand_worktrees: bool) -> RepoList {
    let theme = Arc::new(Theme::default());
    RepoList::new(
        paths.iter().map(PathBuf::from).collect(),
        vec![], // no roots: display falls back to the basename
        expand_worktrees,
        theme,
    )
}

mod render;
mod rows;
