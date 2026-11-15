use std::collections::{HashMap, HashSet};

use super::{DisplayRow, RepoEntry, RepoId, RepoList, RowId};
use crate::git::status::SubmoduleInfo;
use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::ListItem,
};
use std::path::Path;

impl RepoList {
    pub(crate) fn forget_submodule_disclosures(&mut self, repo: &Path) {
        self.collapsed_submodules.retain(|_, owner| owner != repo);
    }

    pub(super) fn render_worktree_item(
        &self,
        entry: &RepoEntry,
        wt_idx: usize,
    ) -> ListItem<'static> {
        let t = &self.theme.repo_list;
        let wt = &entry.status.as_ref().unwrap().worktree_info[wt_idx];
        let mut spans = vec![Span::styled(
            "    \u{2387} ",
            Style::default().fg(t.worktree_subtree_icon),
        )];
        if wt.is_dirty {
            spans.push(Span::styled("* ", Style::default().fg(t.dirty_marker)));
        } else {
            spans.push(Span::raw("  "));
        }
        let count = self.submodule_counts.get(&wt.path).copied().unwrap_or(0);
        if count > 0 {
            spans.push(Span::styled(
                format!("{} ", disclosure(count, self.submodules_expanded(&wt.path))),
                Style::default().fg(t.dirty_submodule),
            ));
        }
        spans.push(Span::styled(
            format!("{:<12} ", wt.branch),
            Style::default().fg(t.worktree_subtree_branch),
        ));
        if wt.ahead > 0 {
            spans.push(Span::styled(
                format!("\u{2191}{} ", wt.ahead),
                Style::default().fg(t.ahead),
            ));
        }
        if wt.behind > 0 {
            spans.push(Span::styled(
                format!("\u{2193}{} ", wt.behind),
                Style::default().fg(t.behind),
            ));
        }
        if wt.has_dirty_submodules {
            spans.push(Span::styled(
                "\u{25c8} ",
                Style::default().fg(t.dirty_submodule),
            ));
        }
        if wt.has_unpushed_submodules {
            spans.push(Span::styled(
                "\u{21e1} ",
                Style::default().fg(t.unpushed_submodule),
            ));
        }
        if wt.file_count > 0 {
            spans.push(Span::styled(
                format!("[{}] ", wt.file_count),
                Style::default().fg(t.file_count),
            ));
        }
        if crate::session::liveness::is_live(&wt.path, &self.live_panes) {
            spans.push(Span::styled("\u{25c9} ", Style::default().fg(t.live)));
        }
        ListItem::new(Line::from(spans))
    }

    pub(super) fn submodules_expanded(&self, path: &Path) -> bool {
        !self.collapsed_submodules.contains_key(path)
    }

    pub(super) fn toggle_selected_submodules(&mut self) -> bool {
        let Some(row) = self.state.selected().and_then(|i| self.display_rows.get(i)) else {
            return false;
        };
        let owner = match *row {
            DisplayRow::Repo(i) => {
                let own = Parent::Repo(i);
                if self.child_count(own) > 0 {
                    Some(own)
                } else {
                    parent(&self.repos, &self.repos[i])
                }
            }
            DisplayRow::Worktree(i, w) | DisplayRow::Submodule(i, Some(w), _) => {
                Some(Parent::Worktree(i, w))
            }
            DisplayRow::Submodule(i, None, _) | DisplayRow::Stash(i, _) => Some(Parent::Repo(i)),
        };
        owner.is_some_and(|owner| self.toggle_submodules(owner))
    }

    fn child_count(&self, owner: Parent) -> usize {
        let path = match owner {
            Parent::Repo(i) => &self.repos[i].path,
            Parent::Worktree(i, w) => &self.repos[i].status.as_ref().unwrap().worktree_info[w].path,
        };
        self.submodule_counts.get(path).copied().unwrap_or(0)
    }

    pub(super) fn toggle_submodules(&mut self, owner: Parent) -> bool {
        if self.child_count(owner) == 0 {
            return false;
        }
        let (path, repo, owner_row) = match owner {
            Parent::Repo(i) => (
                self.repos[i].path.clone(),
                self.repos[i].path.clone(),
                DisplayRow::Repo(i),
            ),
            Parent::Worktree(i, w) => (
                self.repos[i].status.as_ref().unwrap().worktree_info[w]
                    .path
                    .clone(),
                self.repos[i].path.clone(),
                DisplayRow::Worktree(i, w),
            ),
        };
        let previous = self.selected_row_id();
        let collapsing = !self.collapsed_submodules.contains_key(&path);
        if collapsing {
            self.collapsed_submodules.insert(path.clone(), repo);
        } else {
            self.collapsed_submodules.remove(&path);
        }
        self.resync_rows(previous.clone());
        let changed = self.selected_row_id() != previous;
        if collapsing
            && changed
            && let Some(index) = self.display_rows.iter().position(|row| *row == owner_row)
        {
            self.state.select(Some(index));
        }
        self.selected_row_id() != previous
    }

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
                Style::default().fg(self.theme.file_list.submodule_path),
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

pub(super) fn child_counts(repos: &[RepoEntry]) -> HashMap<std::path::PathBuf, usize> {
    let mut paths: HashMap<std::path::PathBuf, HashSet<std::path::PathBuf>> = HashMap::new();
    for entry in repos {
        if let Some(status) = &entry.status {
            for sub in &status.checkout_submodules {
                paths
                    .entry(entry.path.clone())
                    .or_default()
                    .insert(entry.path.join(&sub.path));
            }
            for wt in &status.worktree_info {
                for sub in &wt.checkout_submodules {
                    paths
                        .entry(wt.path.clone())
                        .or_default()
                        .insert(wt.path.join(&sub.path));
                }
            }
        }
        if let Some(owner) = parent(repos, entry) {
            let path = match owner {
                Parent::Repo(i) => &repos[i].path,
                Parent::Worktree(i, w) => &repos[i].status.as_ref().unwrap().worktree_info[w].path,
            };
            paths
                .entry(path.clone())
                .or_default()
                .insert(entry.path.clone());
        }
    }
    paths
        .into_iter()
        .map(|(path, children)| (path, children.len()))
        .collect()
}

pub(super) fn disclosure(count: usize, expanded: bool) -> String {
    format!("{}◈{count}", if expanded { '▾' } else { '▸' })
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
    submodules_expanded: impl Fn(&Path) -> bool,
) -> Vec<DisplayRow> {
    fn append(
        index: usize,
        repos: &[RepoEntry],
        parents: &[Option<Parent>],
        stashes: &HashSet<RepoId>,
        expanded: &impl Fn(&RepoId) -> bool,
        submodules_expanded: &impl Fn(&Path) -> bool,
        rows: &mut Vec<DisplayRow>,
    ) {
        rows.push(DisplayRow::Repo(index));
        if submodules_expanded(&repos[index].path) {
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
                    append(
                        child,
                        repos,
                        parents,
                        stashes,
                        expanded,
                        submodules_expanded,
                        rows,
                    );
                }
            }
        }
        let entry = &repos[index];
        let Some(status) = &entry.status else { return };
        let id = RepoId(entry.path.clone());
        if expanded(&id) {
            for (j, worktree) in status.worktree_info.iter().enumerate() {
                rows.push(DisplayRow::Worktree(index, j));
                if submodules_expanded(&worktree.path) {
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
                            append(
                                child,
                                repos,
                                parents,
                                stashes,
                                expanded,
                                submodules_expanded,
                                rows,
                            );
                        }
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
                &submodules_expanded,
                &mut result,
            );
        }
    }
    result
}
