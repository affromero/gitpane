use crate::config::BranchFilter;
use git2::{BranchType, Oid, Repository};
use std::collections::{HashMap, HashSet};

use super::BranchLabel;

pub(super) fn resolve_refs(
    repo: &Repository,
    filter: &BranchFilter,
) -> HashMap<Oid, Vec<BranchLabel>> {
    if *filter == BranchFilter::None {
        return HashMap::new();
    }

    let head_oid = repo.head().ok().and_then(|r| r.target());
    let head_name = repo
        .head()
        .ok()
        .and_then(|r| r.shorthand().ok().map(String::from));
    let wt_branches = collect_worktree_branches(repo);

    let mut map: HashMap<Oid, Vec<BranchLabel>> = HashMap::new();

    let branch_types: Vec<BranchType> = match filter {
        BranchFilter::All => vec![BranchType::Local, BranchType::Remote],
        BranchFilter::Local => vec![BranchType::Local],
        BranchFilter::Remote => vec![BranchType::Remote],
        BranchFilter::None => unreachable!(),
    };

    for bt in branch_types {
        let branches = match repo.branches(Some(bt)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for branch_result in branches {
            let (branch, _) = match branch_result {
                Ok(b) => b,
                Err(_) => continue,
            };
            let target = match branch.get().target() {
                Some(oid) => oid,
                None => continue,
            };
            let name = match branch.name() {
                Ok(Some(n)) => n.to_string(),
                _ => continue,
            };
            let is_remote = bt == BranchType::Remote;
            let is_head =
                !is_remote && head_oid == Some(target) && head_name.as_deref() == Some(&name);
            let is_worktree = !is_remote && wt_branches.contains(&name);

            map.entry(target).or_default().push(BranchLabel {
                name,
                is_head,
                is_remote,
                is_worktree,
                is_tag: false,
                is_stash: false,
            });
        }
    }

    // Tags
    if let Ok(tag_names) = repo.tag_names(None) {
        for i in 0..tag_names.len() {
            let name = match tag_names.get(i) {
                Ok(Some(n)) => n,
                _ => continue,
            };
            let refname = format!("refs/tags/{}", name);
            let Ok(reference) = repo.find_reference(&refname) else {
                continue;
            };
            let oid = reference
                .peel_to_commit()
                .ok()
                .map(|c| c.id())
                .or_else(|| reference.target());
            if let Some(oid) = oid {
                map.entry(oid).or_default().push(BranchLabel {
                    name: name.to_string(),
                    is_head: false,
                    is_remote: false,
                    is_worktree: false,
                    is_tag: true,
                    is_stash: false,
                });
            }
        }
    }

    // Sort: HEAD first, then local, then remote, then tags, then alphabetical
    for labels in map.values_mut() {
        labels.sort_by(|a, b| {
            b.is_head
                .cmp(&a.is_head)
                .then(a.is_tag.cmp(&b.is_tag))
                .then(a.is_remote.cmp(&b.is_remote))
                .then(a.name.cmp(&b.name))
        });
    }

    map
}

/// Attach a `stash@{n}` label to the commit each stash was created on top of
/// (i.e. the stash entry's first parent). The stash commit itself is not
/// pushed into the revwalk, so its index/untracked tree parents don't
/// pollute the graph. Existing labels for that oid are preserved.
pub(super) fn merge_stash_labels(repo: &mut Repository, map: &mut HashMap<Oid, Vec<BranchLabel>>) {
    let mut stash_entries: Vec<(usize, Oid)> = Vec::new();
    let _ = repo.stash_foreach(|index, _msg, oid| {
        stash_entries.push((index, *oid));
        true
    });

    for (index, oid) in stash_entries {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(parent_oid) = commit.parent_id(0) else {
            continue;
        };
        map.entry(parent_oid).or_default().push(BranchLabel {
            name: format!("stash@{{{index}}}"),
            is_head: false,
            is_remote: false,
            is_worktree: false,
            is_tag: false,
            is_stash: true,
        });
    }
}

fn collect_worktree_branches(repo: &Repository) -> HashSet<String> {
    let mut branches = HashSet::new();
    let wt_names = match repo.worktrees() {
        Ok(names) => names,
        Err(_) => return branches,
    };
    for i in 0..wt_names.len() {
        let name = match wt_names.get(i) {
            Ok(Some(n)) => n,
            _ => continue,
        };
        let wt = match repo.find_worktree(name) {
            Ok(wt) => wt,
            Err(_) => continue,
        };
        let wt_repo = match Repository::open(wt.path()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Ok(head) = wt_repo.head()
            && let Ok(shorthand) = head.shorthand()
        {
            branches.insert(shorthand.to_string());
        }
    }
    branches
}
