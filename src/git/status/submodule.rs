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

fn git_path_key(path: &Path) -> Vec<u8> {
    let bytes = path.as_os_str().as_encoded_bytes().to_vec();
    #[cfg(windows)]
    let bytes = bytes
        .into_iter()
        .map(|byte| if byte == b'\\' { b'/' } else { byte })
        .collect();
    bytes
}

pub(crate) struct SubmoduleRegistration {
    pub(crate) name: Vec<u8>,
    pub(crate) path: PathBuf,
    pub(crate) ignore: git2::SubmoduleIgnore,
    pub(crate) head_oid: Option<git2::Oid>,
    pub(crate) index_oid: Option<git2::Oid>,
}

fn recorded_gitlink_paths(
    repo: &Repository,
    include_conflicts: bool,
) -> Result<std::collections::BTreeSet<PathBuf>, git2::Error> {
    let mut paths = std::collections::BTreeSet::new();
    for entry in repo.index()?.iter() {
        if entry.mode == 0o160000 && (include_conflicts || entry.flags & 0x3000 == 0) {
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
    Ok(paths)
}

fn valid_registration_name(name: &[u8]) -> bool {
    !name.is_empty()
        && !name.contains(&0)
        && !name.starts_with(b"/")
        && name
            .split(|byte| matches!(byte, b'/' | b'\\'))
            .all(|part| !matches!(part, b"" | b"." | b".." | b".git"))
}

fn registration_setting(
    property: &[u8],
    value: Option<&[u8]>,
) -> Result<git2::SubmoduleIgnore, git2::Error> {
    use git2::SubmoduleIgnore;
    let original = value;
    let value = value.map(|value| value.to_ascii_lowercase());
    let value = value.as_deref();
    let boolean = match value {
        None => Some(false),
        Some(value) => std::str::from_utf8(value)
            .ok()
            .and_then(|value| git2::Config::parse_bool(value).ok()),
    };
    match (property, value) {
        (b"ignore", Some(b"none")) => Ok(SubmoduleIgnore::None),
        (b"ignore", Some(b"untracked")) => Ok(SubmoduleIgnore::Untracked),
        (b"ignore", Some(b"dirty")) => Ok(SubmoduleIgnore::Dirty),
        (b"ignore", Some(b"all")) => Ok(SubmoduleIgnore::All),
        (b"update", Some(b"checkout" | b"merge" | b"rebase" | b"none"))
        | (b"fetchrecursesubmodules", Some(b"on-demand")) => Ok(SubmoduleIgnore::None),
        (b"ignore", _) if boolean.is_some() => Ok(if boolean == Some(true) {
            SubmoduleIgnore::All
        } else {
            SubmoduleIgnore::None
        }),
        (b"update" | b"fetchrecursesubmodules", _) if boolean.is_some() => {
            Ok(SubmoduleIgnore::None)
        }
        _ => {
            let decoded_property = String::from_utf8_lossy(property);
            let property = if property == b"fetchrecursesubmodules" {
                "fetchRecurseSubmodules"
            } else {
                decoded_property.as_ref()
            };
            Err(git2::Error::from_str(&format!(
                "invalid value for submodule '{property}' property: '{}'",
                String::from_utf8_lossy(original.unwrap_or_default())
            )))
        }
    }
}

/// Read registration and recorded pointer metadata without inspecting checkouts.
pub(crate) fn submodule_registrations(
    repo: &Repository,
) -> Result<Vec<SubmoduleRegistration>, git2::Error> {
    use std::collections::{BTreeMap, BTreeSet};
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let modules = workdir.join(".gitmodules");
    match modules.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(git2::Error::from_str(&error.to_string())),
    }
    let config = git2::Config::open(&modules)?;
    let mut registrations = BTreeMap::<Vec<u8>, SubmoduleRegistration>::new();
    let mut aliases = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    let mut configured = BTreeSet::new();
    type Settings = BTreeMap<Vec<u8>, Option<Vec<u8>>>;
    let mut settings = BTreeMap::<Vec<u8>, Settings>::new();
    let mut entries = config.entries(None)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let Some(key) = entry.name_bytes().strip_prefix(b"submodule.") else {
            continue;
        };
        let Some(dot) = key.iter().rposition(|byte| *byte == b'.') else {
            continue;
        };
        let (name, property) = key.split_at(dot);
        if !valid_registration_name(name) {
            continue;
        }
        let module = registrations
            .entry(name.to_vec())
            .or_insert(SubmoduleRegistration {
                name: name.to_vec(),
                path: candidate_path(name)?,
                ignore: git2::SubmoduleIgnore::None,
                head_oid: None,
                index_oid: None,
            });
        match property {
            b".path" => {
                if !entry.has_value() {
                    return Err(git2::Error::from_str("Submodule path has no value"));
                }
                let value = entry.value_bytes();
                let path = candidate_path(value)?;
                if aliases.insert(value.to_vec(), name.to_vec()).is_some() {
                    return Err(git2::Error::from_str("Duplicated submodule path"));
                }
                module.path = if value.starts_with(b"-") {
                    candidate_path(name)?
                } else {
                    path
                };
                configured.insert(name.to_vec());
            }
            b".ignore" | b".update" | b".fetchrecursesubmodules" | b".url" | b".branch" => {
                let value = entry.has_value().then(|| entry.value_bytes().to_vec());
                settings
                    .entry(name.to_vec())
                    .or_default()
                    .insert(property[1..].to_vec(), value);
            }
            _ => {}
        }
    }
    for module in registrations.values_mut() {
        if let Some(settings) = settings.get(&module.name) {
            for (property, value) in settings {
                if property == b"url" {
                    if !value
                        .as_deref()
                        .is_some_and(|value| value.starts_with(b"-"))
                    {
                        configured.insert(module.name.clone());
                    }
                } else if property == b"branch" {
                    configured.insert(module.name.clone());
                } else {
                    let setting = registration_setting(property, value.as_deref())?;
                    if property == b"ignore" {
                        module.ignore = setting;
                    }
                    configured.insert(module.name.clone());
                }
            }
        }
    }
    let paths = recorded_gitlink_paths(repo, false)?;
    for path in &paths {
        let path_name = git_path_key(path);
        let name = if registrations.contains_key(&path_name) {
            path_name
        } else {
            aliases.get(&path_name).cloned().unwrap_or(path_name)
        };
        registrations
            .entry(name.clone())
            .or_insert(SubmoduleRegistration {
                name,
                path: path.clone(),
                ignore: git2::SubmoduleIgnore::None,
                head_oid: None,
                index_oid: None,
            });
    }
    let index = repo.index()?;
    let head = match repo.head().and_then(|head| head.peel_to_tree()) {
        Ok(head) => Some(head),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
            ) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    for module in registrations.values_mut() {
        // git2's index wrapper panics for paths it cannot convert. Preserve
        // libgit2's case-aware lookup for accepted paths, and leave rejected
        // spellings unmatched instead of normalizing them into another path.
        let index_path_valid = matches!(
            module.path.components().next(),
            Some(std::path::Component::Normal(_))
        ) && !module.path.as_os_str().as_encoded_bytes().contains(&0);
        module.index_oid = if index_path_valid {
            index
                .get_path(&module.path, 0)
                .filter(|entry| entry.mode & 0o170000 == 0o160000)
                .map(|entry| entry.id)
        } else {
            None
        };
        module.head_oid = head
            .as_ref()
            .and_then(|head| head.get_path(&module.path).ok())
            .filter(|entry| entry.filemode() == 0o160000)
            .map(|entry| entry.id());
        if !configured.contains(&module.name)
            && module.head_oid.is_none()
            && module.index_oid.is_none()
        {
            return Err(git2::Error::from_str(
                "Submodule is not registered in config, HEAD or index",
            ));
        }
    }
    Ok(registrations.into_values().collect())
}

/// Enumerate possible submodule locations without probing their working trees.
/// This conservative preflight includes conflict-stage locations. Use actual
/// registrations, rather than this superset, when collecting child status.
pub(crate) fn submodule_candidate_paths(repo: &Repository) -> Result<Vec<PathBuf>, git2::Error> {
    use std::collections::{BTreeMap, BTreeSet};
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let modules_path = workdir.join(".gitmodules");
    match modules_path.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
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
    paths.extend(recorded_gitlink_paths(repo, true)?);
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

pub(super) struct ChildObservation {
    pub(super) status: git2::SubmoduleStatus,
    pub(super) head_oid: Option<git2::Oid>,
}

pub(super) fn diff_submodule_ignore(
    repo: &Repository,
) -> Result<git2::SubmoduleIgnore, git2::Error> {
    let config = repo.config()?;
    match config.get_entry("diff.ignoresubmodules") {
        // libgit2 resets an invalid diff-level value to None and clears its
        // parse error. Registration-level settings remain validated.
        Ok(entry) if !entry.has_value() => Ok(git2::SubmoduleIgnore::All),
        Ok(entry) => Ok(registration_setting(b"ignore", Some(entry.value_bytes()))
            .unwrap_or(git2::SubmoduleIgnore::None)),
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            Ok(git2::SubmoduleIgnore::Unspecified)
        }
        Err(error) => Err(error),
    }
}

/// Inspect an already opened checkout independently of the parent's
/// submodule cache. Match Git's separate child index/workdir dirty signals.
pub(super) fn observe_child(
    child: &Repository,
    recorded: Option<git2::Oid>,
    ignore: git2::SubmoduleIgnore,
) -> Result<ChildObservation, git2::Error> {
    observe_child_inner(
        child,
        recorded,
        ignore,
        0,
        &mut std::collections::HashSet::new(),
    )
}

fn observe_child_inner(
    child: &Repository,
    recorded: Option<git2::Oid>,
    ignore: git2::SubmoduleIgnore,
    depth: usize,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<ChildObservation, git2::Error> {
    use git2::{DiffOptions, SubmoduleIgnore, SubmoduleStatus};
    let root = child
        .workdir()
        .ok_or_else(|| git2::Error::from_str("Submodule has no working tree"))?;
    let canonical = root
        .canonicalize()
        .map_err(|error| git2::Error::from_str(&error.to_string()))?;
    if depth >= 32 || seen.len() >= 1024 || !seen.insert(canonical) {
        return Err(git2::Error::from_str(
            "Submodule nesting exceeds the inspection limit",
        ));
    }
    let head = match child.head().and_then(|head| head.peel_to_commit()) {
        Ok(head) => Some(head),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let head_oid = head.as_ref().map(|head| head.id());
    let mut status = SubmoduleStatus::empty();
    if ignore == SubmoduleIgnore::All {
        return Ok(ChildObservation { status, head_oid });
    }
    match (recorded, head_oid) {
        (None, Some(_)) => status |= SubmoduleStatus::WD_ADDED,
        (Some(_), None) => status |= SubmoduleStatus::WD_DELETED,
        (Some(old), Some(new)) if old != new => status |= SubmoduleStatus::WD_MODIFIED,
        _ => {}
    }
    if ignore == SubmoduleIgnore::Dirty {
        return Ok(ChildObservation { status, head_oid });
    }
    let index = child.index()?;
    let mut options = DiffOptions::new();
    options.include_untracked(ignore == SubmoduleIgnore::None);
    if let Some(head) = &head {
        let tree = head.tree()?;
        if child
            .diff_tree_to_index(Some(&tree), Some(&index), Some(&mut options))?
            .deltas()
            .len()
            > 0
        {
            status |= SubmoduleStatus::WD_INDEX_MODIFIED;
        }
    }
    options.ignore_submodules(true);
    let working = child.diff_index_to_workdir(Some(&index), Some(&mut options))?;
    for delta in working.deltas() {
        if delta.status() == git2::Delta::Untracked {
            status |= SubmoduleStatus::WD_UNTRACKED;
        } else {
            status |= SubmoduleStatus::WD_WD_MODIFIED;
        }
    }
    // A raw diff observes entries themselves. Inspect registered child
    // checkouts explicitly rather than letting libgit2 populate its cache
    // by following every registered working-tree path.
    let diff_ignore = diff_submodule_ignore(child)?;
    for nested in submodule_registrations(child)? {
        let nested_ignore = if diff_ignore == SubmoduleIgnore::Unspecified {
            nested.ignore
        } else {
            diff_ignore
        };
        if nested.index_oid.is_none() || nested_ignore == SubmoduleIgnore::All {
            continue;
        }
        if nested.path.components().any(|part| {
            !matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) || !nested
            .path
            .components()
            .any(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(git2::Error::from_str(
                "Submodule path must remain inside its parent",
            ));
        }
        let path = root.join(&nested.path);
        if path
            .symlink_metadata()
            .is_ok_and(|entry| entry.file_type().is_symlink())
            || regular_submodule_blocker(&path)
                .map_err(|error| git2::Error::from_str(&error.to_string()))?
                .is_some()
        {
            continue;
        }
        match path.join(".git").symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(git2::Error::from_str(&error.to_string())),
        }
        let repository = Repository::open(&path)?;
        let actual = repository
            .workdir()
            .ok_or_else(|| git2::Error::from_str("Submodule has no working tree"))?
            .canonicalize()
            .map_err(|error| git2::Error::from_str(&error.to_string()))?;
        let expected = path
            .canonicalize()
            .map_err(|error| git2::Error::from_str(&error.to_string()))?;
        if actual != expected {
            return Err(git2::Error::from_str(
                "Submodule path does not identify its own repository",
            ));
        }
        let observation = observe_child_inner(
            &repository,
            nested.index_oid,
            nested_ignore,
            depth + 1,
            seen,
        )?;
        if working_submodule_modified(observation.status) {
            status |= SubmoduleStatus::WD_WD_MODIFIED;
        }
    }
    Ok(ChildObservation { status, head_oid })
}

pub(super) fn working_submodule_modified(status: git2::SubmoduleStatus) -> bool {
    use git2::SubmoduleStatus;
    status.intersects(
        SubmoduleStatus::WD_ADDED
            | SubmoduleStatus::WD_DELETED
            | SubmoduleStatus::WD_MODIFIED
            | SubmoduleStatus::WD_INDEX_MODIFIED
            | SubmoduleStatus::WD_WD_MODIFIED
            | SubmoduleStatus::WD_UNTRACKED,
    )
}

/// Observe a validated child using the parent's staged pointer. Keeping this
/// independent of a libgit2 submodule handle lets callers authorize and open
/// each checkout before inspecting its state.
pub(super) fn submodule_head_and_warn(
    inner: &Repository,
    recorded: Option<git2::Oid>,
    warn_enabled: bool,
) -> (Option<SubmoduleHead>, SubmoduleWarn) {
    let head = Some(read_submodule_head(inner));
    let warn = if warn_enabled {
        compute_submodule_warn(recorded, inner)
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
fn compute_submodule_warn(recorded: Option<git2::Oid>, inner: &Repository) -> SubmoduleWarn {
    let recorded = match recorded {
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
