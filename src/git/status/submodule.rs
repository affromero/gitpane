use git2::Repository;

use super::worktree::compute_ahead_behind;
use super::{SubmoduleHead, SubmoduleWarn};

impl SubmoduleWarn {
    pub fn is_clean(&self) -> bool {
        self.unpushed_commits == 0 && !self.pointer_unreachable && !self.needs_merge_to_default
    }
}

/// Open the submodule once to read its checked-out branch and, when
/// `warn_enabled`, compute its push/merge warnings. Returns `(head, warn)`;
/// `head` is `None` only when the submodule cannot be opened.
pub(super) fn compute_submodule_head_and_warn(
    sub: &git2::Submodule,
    warn_enabled: bool,
) -> (Option<SubmoduleHead>, SubmoduleWarn) {
    let inner = match sub.open() {
        Ok(r) => r,
        Err(_) => return (None, SubmoduleWarn::default()),
    };

    let head = Some(read_submodule_head(&inner));
    let warn = if warn_enabled {
        compute_submodule_warn(sub, &inner)
    } else {
        SubmoduleWarn::default()
    };
    (head, warn)
}

/// Read the submodule's current branch, or `Detached` when it sits on a
/// detached HEAD (git's default after `submodule update`) or has no branch.
fn read_submodule_head(inner: &Repository) -> SubmoduleHead {
    if inner.head_detached().unwrap_or(true) {
        return SubmoduleHead::Detached;
    }
    match inner.head() {
        Ok(head) => SubmoduleHead::Branch(head.shorthand().unwrap_or("HEAD").to_string()),
        Err(_) => SubmoduleHead::Detached,
    }
}

/// Compute "you owe a push/merge" warnings for a submodule, given an already
/// open handle to it. Uses `index_id()` (parent's *staged* pointer, not
/// committed) so the warning fires *before* the parent commit ships, while the
/// user can still amend or reset.
fn compute_submodule_warn(sub: &git2::Submodule, inner: &Repository) -> SubmoduleWarn {
    let recorded = match sub.index_id() {
        Some(o) => o,
        None => return SubmoduleWarn::default(),
    };

    let unpushed_commits = compute_ahead_behind(inner).0;

    // If the recorded oid isn't even in local objects, no remote can possibly hold it.
    if inner.find_object(recorded, None).is_err() {
        return SubmoduleWarn {
            unpushed_commits,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
    }

    if !remote_reaches(inner, recorded) {
        // No remote tip reaches `recorded` (or no remotes configured) → must push.
        return SubmoduleWarn {
            unpushed_commits,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        };
    }

    // Reachable from some remote. If we can resolve the default branch and it
    // does NOT reach `recorded`, the commit lives on a side branch and still
    // needs a merge there. When no default branch resolves, stay silent.
    let needs_merge_to_default = match default_branch_tip(inner) {
        Some(tip) => !reaches(inner, tip, recorded),
        None => false,
    };

    SubmoduleWarn {
        unpushed_commits,
        pointer_unreachable: false,
        needs_merge_to_default,
    }
}

/// Whether `recorded` is reachable from `tip` (equal, or an ancestor of it).
fn reaches(inner: &Repository, tip: git2::Oid, recorded: git2::Oid) -> bool {
    tip == recorded || inner.graph_descendant_of(tip, recorded).unwrap_or(false)
}

/// Whether `recorded` is reachable from any `refs/remotes/*` branch tip.
fn remote_reaches(inner: &Repository, recorded: git2::Oid) -> bool {
    let Ok(branches) = inner.branches(Some(git2::BranchType::Remote)) else {
        return false;
    };
    for (b, _) in branches.flatten() {
        if let Some(tip) = b.get().target()
            && reaches(inner, tip, recorded)
        {
            return true;
        }
    }
    false
}

/// Resolve the submodule's default-branch tip: `origin/HEAD`'s symbolic target
/// first, then `origin/main`, then `origin/master`. `None` when none resolve —
/// common in submodule clones, where `origin/HEAD` is often absent.
fn default_branch_tip(inner: &Repository) -> Option<git2::Oid> {
    if let Ok(head_ref) = inner.find_reference("refs/remotes/origin/HEAD")
        && let Ok(Some(target_name)) = head_ref.symbolic_target()
        && let Ok(target_ref) = inner.find_reference(target_name)
        && let Some(oid) = target_ref.target()
    {
        return Some(oid);
    }
    for name in ["refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if let Ok(r) = inner.find_reference(name)
            && let Some(oid) = r.target()
        {
            return Some(oid);
        }
    }
    None
}
