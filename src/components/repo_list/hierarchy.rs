use std::collections::HashSet;

use super::{DisplayRow, RepoEntry, RepoId, RepoList, RowId};
use crate::git::status::SubmoduleInfo;
use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::ListItem,
};
use std::path::Path;

impl RepoList {
    /// Identify the selection across sorting and status updates.
    pub fn selected_row_id(&self) -> Option<RowId> {
        let di = self.state.selected()?;
        match self.display_rows.get(di)? {
            DisplayRow::Repo(i) => Some(RowId::Repo(self.repos.get(*i)?.path.clone())),
            DisplayRow::Worktree(ri, wi) => {
                Some(RowId::Worktree(self.repos.get(*ri)?.path.clone(), *wi))
            }
            DisplayRow::Submodule(ri, wi, si) => {
                let (parent, submodule) = self.submodule_at(*ri, *wi, *si)?;
                Some(RowId::Submodule(
                    self.repos.get(*ri)?.path.clone(),
                    parent.join(&submodule.path),
                ))
            }
            DisplayRow::Stash(ri, si) => {
                let entry = self.repos.get(*ri)?;
                let oid = entry.status.as_ref()?.stashes.get(*si)?.oid.clone();
                Some(RowId::Stash(entry.path.clone(), oid))
            }
        }
    }

    pub fn selected_submodule_uninitialized(&self) -> bool {
        let Some(DisplayRow::Submodule(repo, worktree, index)) = self
            .state
            .selected()
            .and_then(|selected| self.display_rows.get(selected))
        else {
            return false;
        };
        self.submodule_at(*repo, *worktree, *index)
            .is_some_and(|(_, submodule)| {
                submodule.state == Some(crate::git::status::SubmoduleState::Uninitialized)
            })
    }

    pub fn selected_submodule_path(&self) -> Option<std::path::PathBuf> {
        let selected = self.state.selected()?;
        let DisplayRow::Submodule(repo, worktree, index) = self.display_rows.get(selected)? else {
            return None;
        };
        let (parent, submodule) = self.submodule_at(*repo, *worktree, *index)?;
        Some(parent.join(&submodule.path))
    }

    pub(super) fn submodule_at(
        &self,
        repo: usize,
        worktree: Option<usize>,
        submodule: usize,
    ) -> Option<(&Path, &SubmoduleInfo)> {
        let entry = self.repos.get(repo)?;
        let status = entry.status.as_ref()?;
        match worktree {
            Some(index) => {
                let worktree = status.worktree_info.get(index)?;
                Some((&worktree.path, worktree.checkout_submodules.get(submodule)?))
            }
            None => Some((&entry.path, status.checkout_submodules.get(submodule)?)),
        }
    }

    pub(super) fn render_submodule_item(
        &self,
        repo: usize,
        worktree: Option<usize>,
        index: usize,
    ) -> ListItem<'static> {
        let Some((_, submodule)) = self.submodule_at(repo, worktree, index) else {
            return ListItem::new("");
        };
        let theme = &self.theme.repo_list;
        let indent = if worktree.is_some() { "      " } else { "  " };
        let marker = if submodule.state.is_some() {
            "* "
        } else {
            "  "
        };
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("{indent}└ "),
                Style::default().fg(theme.worktree_subtree_icon),
            ),
            Span::styled(marker, Style::default().fg(theme.dirty_marker)),
            Span::styled(
                submodule.path.display().to_string(),
                Style::default().fg(theme.repo_name),
            ),
            Span::styled(" [sub]", Style::default().fg(theme.worktree_subtree_icon)),
        ]))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Parent {
    Repo(usize),
    Worktree(usize, usize),
}

/// Attach a pinned checkout to the deepest listed repository or worktree.
pub(super) fn parent(repos: &[RepoEntry], entry: &RepoEntry) -> Option<Parent> {
    if !entry.gitlink {
        return None;
    }
    let mut best = None;
    let mut depth = 0;
    for (i, candidate) in repos.iter().enumerate() {
        if candidate.path != entry.path && entry.path.starts_with(&candidate.path) {
            let current = candidate.path.components().count();
            if current > depth {
                depth = current;
                best = Some(Parent::Repo(i));
            }
        }
        // A worktree nested inside the child cannot own that child. Excluding
        // that case also keeps the resulting hierarchy acyclic.
        if candidate.path.starts_with(&entry.path) {
            continue;
        }
        if let Some(status) = &candidate.status {
            for (j, worktree) in status.worktree_info.iter().enumerate() {
                if worktree.path != entry.path && entry.path.starts_with(&worktree.path) {
                    let current = worktree.path.components().count();
                    if current > depth {
                        depth = current;
                        best = Some(Parent::Worktree(i, j));
                    }
                }
            }
        }
    }
    best
}

pub(super) fn rows(
    repos: &[RepoEntry],
    expanded_stashes: &HashSet<RepoId>,
    worktrees_expanded: impl Fn(&RepoId) -> bool,
) -> Vec<DisplayRow> {
    fn append(
        index: usize,
        repos: &[RepoEntry],
        parents: &[Option<Parent>],
        stashes: &HashSet<RepoId>,
        expanded: &impl Fn(&RepoId) -> bool,
        rows: &mut Vec<DisplayRow>,
    ) {
        rows.push(DisplayRow::Repo(index));
        if let Some(status) = &repos[index].status {
            for (sub, info) in status.checkout_submodules.iter().enumerate() {
                if !repos.iter().enumerate().any(|(child, entry)| {
                    entry.path == repos[index].path.join(&info.path)
                        && parents[child] == Some(Parent::Repo(index))
                }) {
                    rows.push(DisplayRow::Submodule(index, None, sub));
                }
            }
        }
        for (child, parent) in parents.iter().enumerate() {
            if *parent == Some(Parent::Repo(index)) {
                append(child, repos, parents, stashes, expanded, rows);
            }
        }
        let entry = &repos[index];
        let Some(status) = &entry.status else { return };
        let id = RepoId(entry.path.clone());
        if expanded(&id) {
            for (j, worktree) in status.worktree_info.iter().enumerate() {
                rows.push(DisplayRow::Worktree(index, j));
                for (sub, info) in worktree.checkout_submodules.iter().enumerate() {
                    if !repos.iter().enumerate().any(|(child, entry)| {
                        entry.path == worktree.path.join(&info.path)
                            && parents[child] == Some(Parent::Worktree(index, j))
                    }) {
                        rows.push(DisplayRow::Submodule(index, Some(j), sub));
                    }
                }
                for (child, parent) in parents.iter().enumerate() {
                    if *parent == Some(Parent::Worktree(index, j)) {
                        append(child, repos, parents, stashes, expanded, rows);
                    }
                }
            }
        }
        if stashes.contains(&id) {
            for j in 0..status.stashes.len() {
                rows.push(DisplayRow::Stash(index, j));
            }
        }
    }
    let mut parents: Vec<_> = repos.iter().map(|entry| parent(repos, entry)).collect();
    for index in 0..parents.len() {
        let mut seen = HashSet::new();
        let mut current = index;
        while seen.insert(current) {
            current = match parents[current] {
                Some(Parent::Repo(owner) | Parent::Worktree(owner, _)) => owner,
                None => break,
            };
        }
        if parents[current].is_some() && seen.contains(&current) {
            parents[current] = None;
        }
    }
    let mut result = Vec::new();
    for (index, parent) in parents.iter().enumerate() {
        if parent.is_none() {
            append(
                index,
                repos,
                &parents,
                expanded_stashes,
                &worktrees_expanded,
                &mut result,
            );
        }
    }
    result
}
