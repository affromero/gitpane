use git2::Repository;
use std::io;
use std::path::{Path, PathBuf};

use super::worktree::compute_ahead_behind;
use super::{SubmoduleHead, SubmoduleWarn};

fn candidate_path(bytes: &[u8]) -> Result<PathBuf, git2::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Path::new(std::ffi::OsStr::from_bytes(bytes)).to_path_buf())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| git2::Error::from_str("Invalid submodule path encoding"))
    }
}

/// Enumerate possible submodule locations without probing their working trees.
/// The caller must still use libgit2's submodule enumeration after authorizing
/// these paths; this preflight deliberately retains a superset of locations.
pub(crate) fn submodule_candidate_paths(repo: &Repository) -> Result<Vec<PathBuf>, git2::Error> {
    use std::collections::{BTreeMap, BTreeSet};
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let modules_path = workdir.join(".gitmodules");
    match modules_path.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(git2::Error::from_str(&error.to_string())),
    }
    let config = match git2::Config::open(&modules_path) {
        Ok(config) => config,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    let mut entries = config.entries(None)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let key = entry.name_bytes();
        if let Some(key) = key.strip_prefix(b"submodule.")
            && let Some(dot) = key.iter().rposition(|byte| *byte == b'.')
        {
            let (name, property) = key.split_at(dot);
            if !name.is_empty()
                && candidate_path(name)?
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                let path = names.entry(name.to_vec()).or_insert_with(|| name.to_vec());
                if property == b".path" {
                    if !entry.has_value() {
                        return Err(git2::Error::from_str("Submodule path has no value"));
                    }
                    *path = if entry.value_bytes().starts_with(b"-") {
                        name.to_vec()
                    } else {
                        entry.value_bytes().to_vec()
                    };
                }
            }
        }
    }
    let mut paths = BTreeSet::new();
    for path in names.values() {
        paths.insert(candidate_path(path)?);
    }
    for entry in repo.index()?.iter() {
        if entry.mode == 0o160000 {
            paths.insert(candidate_path(&entry.path)?);
        }
    }
    match repo.head().and_then(|head| head.peel_to_tree()) {
        Ok(tree) => {
            let mut pending = vec![(PathBuf::new(), tree)];
            while let Some((root, tree)) = pending.pop() {
                for entry in &tree {
                    let path = root.join(candidate_path(entry.name_bytes())?);
                    if entry.filemode() == 0o160000 {
                        paths.insert(path);
                    } else if entry.kind() == Some(git2::ObjectType::Tree) {
                        pending.push((path, repo.find_tree(entry.id())?));
                    }
                }
            }
        }
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) => {}
        Err(error) => return Err(error),
    }
    Ok(paths.into_iter().collect())
}

/// Identify a regular file blocking a submodule checkout without following
/// the blocking leaf. Workspace authorization remains the caller's job.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RegularSubmoduleBlocker {
    pub(crate) path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

pub(crate) fn regular_submodule_blocker(
    path: &Path,
) -> io::Result<Option<RegularSubmoduleBlocker>> {
    let mut candidate = path;
    let mut blocked = false;
    loop {
        match candidate.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                #[cfg(not(unix))]
                {
                    let _ = metadata;
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Submodule paths blocked by files are unavailable on this platform.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let canonical = candidate.canonicalize()?;
                    let current = candidate.symlink_metadata()?;
                    if !current.file_type().is_file()
                        || current.dev() != metadata.dev()
                        || current.ino() != metadata.ino()
                        || candidate.canonicalize()? != canonical
                    {
                        return Err(io::Error::other(
                            "Submodule blocker changed while validating.",
                        ));
                    }
                    return Ok(Some(RegularSubmoduleBlocker {
                        path: canonical,
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    }));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                blocked = true;
                candidate = candidate
                    .parent()
                    .ok_or_else(|| io::Error::other("Submodule path has no ancestor."))?;
            }
            Ok(_) if blocked => {
                return Err(io::Error::other(
                    "Submodule blocker changed while validating.",
                ));
            }
            Err(error) if blocked || error.kind() != io::ErrorKind::NotFound => return Err(error),
            Ok(_) | Err(_) => return Ok(None),
        }
    }
}

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
    strict: bool,
) -> color_eyre::Result<(Option<SubmoduleHead>, SubmoduleWarn)> {
    let inner = match sub.open() {
        Ok(r) => r,
        Err(error) if strict => return Err(error.into()),
        Err(_) => return Ok((None, SubmoduleWarn::default())),
    };

    let head = Some(read_submodule_head(&inner));
    let warn = if warn_enabled {
        compute_submodule_warn(sub, &inner)
    } else {
        SubmoduleWarn::default()
    };
    Ok((head, warn))
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
