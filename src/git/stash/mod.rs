//! Read immutable stash snapshots without consulting the index or working tree.

use std::path::Path;

use color_eyre::eyre::{Result, bail, eyre};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository, Tree};

mod types;
pub(crate) use types::{
    StashContent, StashContentKind, StashFile, StashFileContent, StashFiles, StashSource,
};

#[cfg(test)]
mod cli_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

struct Snapshot<'repo> {
    oid: Oid,
    message: String,
    parent_oid: Oid,
    untracked_parent_oid: Option<Oid>,
    base: Tree<'repo>,
    saved: Tree<'repo>,
    untracked: Option<Tree<'repo>>,
}

fn full_oid(value: &str) -> Result<Oid> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("A full 40-character stash object ID is required.");
    }
    Ok(Oid::from_str(value)?)
}

fn object_repository(path: &Path) -> Result<Repository> {
    // Resolve the ODB through the source repository so linked worktrees and alternates
    // retain their object lookup rules. Reopening its gitdir as bare would still expose
    // the source index and info/attributes to libgit2's patch attribute lookup.
    let source = Repository::open(path)?;
    let repo = Repository::from_odb(source.odb()?)?;
    repo.set_config(&git2::Config::new()?)?;
    let mut index = git2::Index::new()?;
    repo.set_index(&mut index)?;
    Ok(repo)
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.as_bytes().contains(&0)
        && !Path::new(path).is_absolute()
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && (!cfg!(windows) || !path.contains('\\'))
        && (!cfg!(windows) || path.as_bytes().get(1).is_none_or(|byte| *byte != b':'))
}

fn snapshot(repo: &Repository, oid: Oid) -> Result<Snapshot<'_>> {
    let commit = repo.find_commit(oid)?;
    if !matches!(commit.parent_count(), 2 | 3) {
        bail!("The requested object does not have a stash's two or three parents.");
    }
    let parent = commit.parent(0)?;
    let untracked = if commit.parent_count() == 3 {
        Some(commit.parent(2)?)
    } else {
        None
    };
    Ok(Snapshot {
        oid,
        message: commit.message().unwrap_or("").trim().to_string(),
        parent_oid: parent.id(),
        untracked_parent_oid: untracked.as_ref().map(git2::Commit::id),
        base: parent.tree()?,
        saved: commit.tree()?,
        untracked: untracked.as_ref().map(git2::Commit::tree).transpose()?,
    })
}

impl Snapshot<'_> {
    fn trees(&self, source: StashSource) -> Result<(Option<&Tree<'_>>, &Tree<'_>)> {
        match source {
            StashSource::Tracked => Ok((Some(&self.base), &self.saved)),
            StashSource::SavedUntracked => self
                .untracked
                .as_ref()
                .map(|tree| (None, tree))
                .ok_or_else(|| eyre!("This stash has no saved-untracked parent.")),
        }
    }
}

fn tree_diff<'repo>(
    repo: &'repo Repository,
    old: Option<&Tree<'repo>>,
    new: &Tree<'repo>,
) -> Result<Diff<'repo>> {
    // Binary classification comes from the saved blob bytes below. The independent
    // object repository isolates attribute lookup; force_text leaves classification to us.
    let mut options = DiffOptions::new();
    options
        .force_text(true)
        .include_typechange(true)
        .ignore_filemode(false)
        .ignore_submodules(false)
        .context_lines(3)
        .interhunk_lines(0)
        .id_abbrev(7);
    let mut diff = repo.diff_tree_to_tree(old, Some(new), Some(&mut options))?;
    diff.find_similar(Some(
        DiffFindOptions::new()
            .renames(true)
            .copies(false)
            .rename_threshold(50)
            .rename_limit(1000),
    ))?;
    Ok(diff)
}

fn file_entry(delta: &git2::DiffDelta<'_>, source: StashSource) -> Result<StashFile> {
    let status = match delta.status() {
        Delta::Added => "A",
        Delta::Deleted => "D",
        Delta::Modified => "M",
        Delta::Renamed => "R",
        Delta::Typechange => "T",
        _ => {
            bail!("Unsupported stash file delta.");
        }
    };
    let path = |file: git2::DiffFile<'_>| -> Result<String> {
        let path = file
            .path()
            .ok_or_else(|| eyre!("A stash file path is missing."))?;
        let encoded = encoded_path(path)?;
        if !valid_path(&encoded) {
            bail!("A stash file path is invalid.");
        }
        Ok(encoded)
    };
    let old_path = (delta.status() != Delta::Added)
        .then(|| path(delta.old_file()))
        .transpose()?;
    let new_path = (delta.status() != Delta::Deleted)
        .then(|| path(delta.new_file()))
        .transpose()?;
    Ok(StashFile {
        source,
        status: status.to_string(),
        path: new_path
            .as_ref()
            .or(old_path.as_ref())
            .ok_or_else(|| eyre!("A stash file has neither an old nor a new path."))?
            .clone(),
        old_path,
        new_path,
        is_binary: false,
        is_submodule: delta.old_file().mode() == git2::FileMode::Commit
            || delta.new_file().mode() == git2::FileMode::Commit,
    })
}

fn encoded_path(path: &Path) -> Result<String> {
    // Reserve the prefix for escaped raw bytes, including when a literal path
    // starts with it, so distinct Git paths cannot share a preview identity.
    const PREFIX: &str = "~gitpane-raw~";
    if let Some(text) = path.to_str()
        && !text.starts_with(PREFIX)
    {
        return Ok(text.to_string());
    }
    #[cfg(unix)]
    {
        use std::fmt::Write;
        use std::os::unix::ffi::OsStrExt;
        let mut result = PREFIX.to_string();
        for byte in path.as_os_str().as_bytes() {
            write!(result, "{byte:02x}")?;
        }
        Ok(result)
    }
    #[cfg(not(unix))]
    {
        path.to_str()
            .map(str::to_string)
            .ok_or_else(|| eyre!("A stash file path is not valid UTF-8."))
    }
}

fn content(
    repo: &Repository,
    tree: Option<&Tree<'_>>,
    path: Option<&Path>,
) -> Result<StashContent> {
    let Some((tree, path)) = tree.zip(path) else {
        return Ok(StashContent {
            kind: StashContentKind::Missing,
            oid: None,
            mode: None,
            size: 0,
            text: Some(String::new()),
        });
    };
    let entry = tree.get_path(path)?;
    let mode = entry.filemode() as u32;
    let (kind, size, text) = match mode {
        0o160000 => {
            // A gitlink need not exist in this repository's object database.
            let text = format!("Subproject commit {}\n", entry.id());
            (StashContentKind::Gitlink, text.len(), Some(text))
        }
        0o100644 | 0o100664 | 0o100755 | 0o120000 => {
            let blob = repo.find_blob(entry.id())?;
            match std::str::from_utf8(blob.content()) {
                Ok(text) if !blob.is_binary() => {
                    (StashContentKind::Text, blob.size(), Some(text.to_string()))
                }
                _ => (StashContentKind::Binary, blob.size(), None),
            }
        }
        _ => {
            bail!("Unsupported stash tree entry mode: {mode:o}.");
        }
    };
    Ok(StashContent {
        kind,
        oid: Some(entry.id().to_string()),
        mode: Some(mode),
        size,
        text,
    })
}

fn sides(
    repo: &Repository,
    old_tree: Option<&Tree<'_>>,
    new_tree: &Tree<'_>,
    file: &mut StashFile,
    delta: &git2::DiffDelta<'_>,
) -> Result<(StashContent, StashContent)> {
    let old_file = delta.old_file();
    let new_file = delta.new_file();
    let old = content(
        repo,
        old_tree,
        file.old_path.as_ref().and_then(|_| old_file.path()),
    )?;
    let new = content(
        repo,
        Some(new_tree),
        file.new_path.as_ref().and_then(|_| new_file.path()),
    )?;
    file.is_binary = old.kind == StashContentKind::Binary || new.kind == StashContentKind::Binary;
    Ok((old, new))
}

/// List the combined tracked changes and the files saved in the optional third parent.
pub(crate) fn list_stash_files(path: &Path, oid: &str) -> Result<StashFiles> {
    let oid = full_oid(oid)?;
    let repo = object_repository(path)?;
    let snapshot = snapshot(&repo, oid)?;
    let mut files = Vec::new();
    for source in [StashSource::Tracked, StashSource::SavedUntracked] {
        if source == StashSource::SavedUntracked && snapshot.untracked.is_none() {
            continue;
        }
        let (old, new) = snapshot.trees(source)?;
        let diff = tree_diff(&repo, old, new)?;
        for delta in diff.deltas() {
            let mut file = file_entry(&delta, source)?;
            sides(&repo, old, new, &mut file, &delta)?;
            files.push(file);
        }
    }
    Ok(StashFiles {
        oid: snapshot.oid.to_string(),
        message: snapshot.message,
        parent_oid: snapshot.parent_oid.to_string(),
        untracked_parent_oid: snapshot.untracked_parent_oid.map(|oid| oid.to_string()),
        files,
    })
}

fn patch_text(diff: &Diff<'_>, index: usize, file: &StashFile) -> Result<String> {
    if file.is_binary {
        let selected = diff
            .get_delta(index)
            .ok_or_else(|| eyre!("The stash file delta is unavailable."))?;
        // Print only Git's headers; binary bytes must never pass through a text decoder.
        let mut headers = Vec::new();
        diff.print(git2::DiffFormat::PatchHeader, |delta, _, line| {
            if delta.old_file().path() == selected.old_file().path()
                && delta.new_file().path() == selected.new_file().path()
            {
                headers.extend_from_slice(line.content());
            }
            true
        })?;
        let mut output = String::from_utf8(headers)?;
        if selected.old_file().id() != selected.new_file().id() {
            output.push_str("Binary files differ\n");
        }
        return Ok(output);
    }
    let mut patch = Patch::from_diff(diff, index)?
        .ok_or_else(|| eyre!("The stash text patch is unavailable."))?;
    let buffer = patch.to_buf()?;
    Ok(String::from_utf8(buffer.to_vec())?)
}

/// Read both saved sides of exactly one (source, path) returned by list_stash_files.
pub(crate) fn stash_file_content(
    path: &Path,
    oid: &str,
    source: StashSource,
    file_path: &str,
) -> Result<StashFileContent> {
    let oid = full_oid(oid)?;
    if !valid_path(file_path) {
        bail!("A literal repository-relative stash file path is required.");
    }
    let repo = object_repository(path)?;
    let snapshot = snapshot(&repo, oid)?;
    let (old_tree, new_tree) = snapshot.trees(source)?;
    let diff = tree_diff(&repo, old_tree, new_tree)?;
    for (index, delta) in diff.deltas().enumerate() {
        let mut file = file_entry(&delta, source)?;
        if file.path != file_path {
            continue;
        }
        let (old, new) = sides(&repo, old_tree, new_tree, &mut file, &delta)?;
        let diff = patch_text(&diff, index, &file)?;
        return Ok(StashFileContent {
            oid: snapshot.oid.to_string(),
            file,
            old,
            new,
            diff,
        });
    }
    bail!("The requested file was not found in this stash source.");
}

/// Return the same object-backed patch used by the bridge for the TUI diff viewer.
pub(crate) fn stash_file_diff(
    path: &Path,
    oid: &str,
    source: StashSource,
    file_path: &str,
) -> Result<String> {
    Ok(stash_file_content(path, oid, source, file_path)?.diff)
}
