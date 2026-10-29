use git2::{Diff, DiffOptions, Oid, Repository};
use std::path::Path;

/// List files changed in a commit (vs its first parent, or empty tree for root).
/// Returns the full commit message and `(status_label, file_path)` pairs.
pub(crate) fn list_commit_files(
    path: &Path,
    oid_str: &str,
) -> color_eyre::Result<(String, Vec<(String, String)>)> {
    let (message, _, files) = list_commit_files_inner(path, oid_str, false)?;
    Ok((
        message,
        files
            .into_iter()
            .map(|file| (file.status, file.path))
            .collect(),
    ))
}

pub(crate) struct CommitFile {
    pub status: String,
    pub path: String,
    pub old_path: Option<String>,
    pub is_submodule: bool,
    pub gitlink: Option<GitlinkDelta>,
}

pub(crate) struct GitlinkDelta {
    pub old: Option<GitlinkSide>,
    pub new: Option<GitlinkSide>,
}

pub(crate) struct GitlinkSide {
    pub kind: &'static str,
    pub oid: Oid,
    pub mode: i32,
}

fn gitlink_side(
    file: git2::DiffFile<'_>,
    tree: Option<&git2::Tree<'_>>,
) -> color_eyre::Result<Option<GitlinkSide>> {
    let Some(tree) = tree else { return Ok(None) };
    let path = file
        .path()
        .ok_or_else(|| color_eyre::eyre::eyre!("committed file has no path"))?;
    let entry = match tree.get_path(path) {
        Ok(entry) => entry,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mode = entry.filemode();
    let kind = match mode {
        0o160000 => "gitlink",
        0o100644 | 0o100664 | 0o100755 => "file",
        0o120000 => "symlink",
        0o040000 => "tree",
        _ => {
            return Err(color_eyre::eyre::eyre!("invalid committed file mode"));
        }
    };
    if entry.id().is_zero() {
        return Err(color_eyre::eyre::eyre!(
            "committed file has an invalid object ID"
        ));
    }
    Ok(Some(GitlinkSide {
        kind,
        oid: entry.id(),
        mode,
    }))
}

pub(crate) fn list_commit_files_detailed(
    path: &Path,
    oid_str: &str,
) -> color_eyre::Result<(String, Option<Oid>, Vec<CommitFile>)> {
    list_commit_files_inner(path, oid_str, true)
}

fn list_commit_files_inner(
    path: &Path,
    oid_str: &str,
    detect_renames: bool,
) -> color_eyre::Result<(String, Option<Oid>, Vec<CommitFile>)> {
    let repo = Repository::open(path)?;
    let oid = Oid::from_str(oid_str)?;
    let commit = repo.find_commit(oid)?;

    let message = commit.message().unwrap_or("").trim().to_string();

    let tree = commit.tree()?;
    let parent = if commit.parent_count() == 0 {
        None
    } else {
        Some(commit.parent(0)?)
    };
    let parent_oid = parent.as_ref().map(git2::Commit::id);
    let parent_tree = parent.as_ref().map(git2::Commit::tree).transpose()?;

    let mut options = DiffOptions::new();
    if detect_renames {
        options
            .include_typechange(true)
            .include_typechange_trees(true);
    }
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))?;
    if detect_renames {
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
    }

    let files = changed_tree_files(&diff, parent_tree.as_ref(), Some(&tree))?;
    Ok((message, parent_oid, files))
}

fn changed_tree_files(
    diff: &Diff<'_>,
    old_tree: Option<&git2::Tree<'_>>,
    new_tree: Option<&git2::Tree<'_>>,
) -> color_eyre::Result<Vec<CommitFile>> {
    let mut files = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified | git2::Delta::Typechange => "M",
            git2::Delta::Renamed => "R",
            _ => "?",
        };
        let file_path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .ok_or_else(|| color_eyre::eyre::eyre!("commit file has no path"))?
            .to_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("commit file path is not valid UTF-8"))?
            .to_string();
        let old_path = if delta.status() == git2::Delta::Renamed {
            Some(
                delta
                    .old_file()
                    .path()
                    .and_then(Path::to_str)
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!("original commit path is not valid UTF-8")
                    })?
                    .to_string(),
            )
        } else {
            None
        };
        let is_submodule = matches!(delta.old_file().mode(), git2::FileMode::Commit)
            || matches!(delta.new_file().mode(), git2::FileMode::Commit);
        let gitlink = if is_submodule {
            Some(GitlinkDelta {
                old: gitlink_side(delta.old_file(), old_tree)?,
                new: gitlink_side(delta.new_file(), new_tree)?,
            })
        } else {
            None
        };
        files.push(CommitFile {
            status: status.to_string(),
            path: file_path,
            old_path,
            is_submodule,
            gitlink,
        });
    }

    Ok(files)
}

/// Compare committed HEAD against the merge base with the configured review base.
pub(crate) fn list_review_files(
    path: &Path,
    configured_base: Option<&str>,
) -> color_eyre::Result<(String, Oid, Oid, Vec<CommitFile>)> {
    let repo = Repository::open(path)?;
    let base_ref = configured_base
        .map(str::to_owned)
        .or_else(|| crate::git::status::default_branch_name(&repo))
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("No review base resolved; set [review] base in config")
        })?;
    let base = repo.revparse_single(&base_ref)?.peel_to_commit()?;
    let head = repo.head()?.peel_to_commit()?;
    let merge_base = repo.merge_base(base.id(), head.id())?;
    let base_tree = repo.find_commit(merge_base)?.tree()?;
    let head_tree = head.tree()?;
    let mut diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;
    diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
    Ok((
        base_ref,
        merge_base,
        head.id(),
        changed_tree_files(&diff, Some(&base_tree), Some(&head_tree))?,
    ))
}

/// Compare two committed trees, substituting an empty tree for either missing side.
pub(crate) fn list_revision_files(
    path: &Path,
    base: Option<Oid>,
    head: Option<Oid>,
) -> color_eyre::Result<Vec<CommitFile>> {
    if base.is_none() && head.is_none() {
        return Ok(Vec::new());
    }
    let repo = Repository::open(path)?;
    let base_tree = base.map(|oid| repo.find_commit(oid)?.tree()).transpose()?;
    let head_tree = head.map(|oid| repo.find_commit(oid)?.tree()).transpose()?;
    let mut diff = repo.diff_tree_to_tree(base_tree.as_ref(), head_tree.as_ref(), None)?;
    diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
    changed_tree_files(&diff, base_tree.as_ref(), head_tree.as_ref())
}

/// Get the diff text for a single file in a commit.
pub(crate) fn commit_file_diff(
    path: &Path,
    oid_str: &str,
    file_path: &str,
) -> color_eyre::Result<String> {
    let repo = Repository::open(path)?;
    let oid = Oid::from_str(oid_str)?;
    let commit = repo.find_commit(oid)?;

    let tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let mut opts = DiffOptions::new();
    opts.pathspec(file_path).disable_pathspec_match(true);

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

    let mut output = String::new();
    diff_to_string(&diff, &mut output)?;

    if output.is_empty() {
        output = "(no diff available)".to_string();
    }

    Ok(output)
}

use crate::git::graph::DiffStat;

/// Compute diff stats (additions/deletions) for a batch of commits.
pub(crate) fn batch_diff_stats(
    path: &Path,
    oids: &[Oid],
) -> color_eyre::Result<Vec<(Oid, DiffStat)>> {
    let repo = Repository::open(path)?;
    let mut results = Vec::with_capacity(oids.len());
    for &oid in oids {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(tree) = commit.tree() else {
            continue;
        };
        let parent_tree = if commit.parent_count() == 0 {
            None
        } else {
            Some(commit.parent(0)?.tree()?)
        };
        let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
            continue;
        };
        let Ok(stats) = diff.stats() else {
            continue;
        };
        results.push((
            oid,
            DiffStat {
                additions: stats.insertions(),
                deletions: stats.deletions(),
            },
        ));
    }
    Ok(results)
}

pub(super) fn diff_to_string(diff: &Diff<'_>, output: &mut String) -> color_eyre::Result<()> {
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            _ => "",
        };
        output.push_str(prefix);
        output.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use tempfile::TempDir;

    fn committed_tree(repo: &Repository, parent: Option<Oid>, entries: &[(&str, Oid, i32)]) -> Oid {
        let mut builder = repo.treebuilder(None).unwrap();
        for (path, oid, mode) in entries {
            builder.insert(path, *oid, *mode).unwrap();
        }
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = Signature::now("Test", "test@example.invalid").unwrap();
        let parent = parent.map(|oid| repo.find_commit(oid).unwrap());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(None, &signature, &signature, "fixture", &tree, &parents)
            .unwrap()
    }

    fn single_detailed(repo: &Repository, oid: Oid) -> CommitFile {
        let (_, _, mut files) =
            list_commit_files_detailed(repo.workdir().unwrap(), &oid.to_string()).unwrap();
        assert_eq!(files.len(), 1);
        files.remove(0)
    }

    fn assert_side(side: &Option<GitlinkSide>, kind: &str, oid: Oid, mode: i32) {
        let side = side.as_ref().unwrap();
        assert_eq!((side.kind, side.oid, side.mode), (kind, oid, mode));
    }

    #[test]
    fn gitlink_root_add_modify_and_delete_use_commit_tree_pointers() {
        let (_temp, repo, first) = create_repo_with_file("base", "content");
        let first = Oid::from_str(&first).unwrap();
        let second = committed_tree(&repo, Some(first), &[]);
        let root = committed_tree(&repo, None, &[("child", first, 0o160000)]);
        let root_file = single_detailed(&repo, root);
        assert_eq!(root_file.status, "A");
        let root_delta = root_file.gitlink.unwrap();
        assert!(root_delta.old.is_none());
        assert_side(&root_delta.new, "gitlink", first, 0o160000);
        let empty = committed_tree(&repo, None, &[]);
        let added = committed_tree(&repo, Some(empty), &[("child", first, 0o160000)]);
        let added_delta = single_detailed(&repo, added).gitlink.unwrap();
        assert!(added_delta.old.is_none());
        assert_side(&added_delta.new, "gitlink", first, 0o160000);
        let modified = committed_tree(&repo, Some(added), &[("child", second, 0o160000)]);
        let modified_delta = single_detailed(&repo, modified).gitlink.unwrap();
        assert_side(&modified_delta.old, "gitlink", first, 0o160000);
        assert_side(&modified_delta.new, "gitlink", second, 0o160000);
        let deleted = committed_tree(&repo, Some(modified), &[]);
        let deleted_file = single_detailed(&repo, deleted);
        assert_eq!(deleted_file.status, "D");
        let deleted_delta = deleted_file.gitlink.unwrap();
        assert_side(&deleted_delta.old, "gitlink", second, 0o160000);
        assert!(deleted_delta.new.is_none());
    }

    #[test]
    fn moved_gitlink_reports_canonical_deleted_and_added_entries() {
        let (_temp, repo, pointer) = create_repo_with_file("base", "content");
        let pointer = Oid::from_str(&pointer).unwrap();
        let before = committed_tree(&repo, None, &[("old", pointer, 0o160000)]);
        let after = committed_tree(&repo, Some(before), &[("new", pointer, 0o160000)]);
        let (_, _, files) =
            list_commit_files_detailed(repo.workdir().unwrap(), &after.to_string()).unwrap();
        assert_eq!(files.len(), 2);
        let old = files.iter().find(|file| file.path == "old").unwrap();
        let new = files.iter().find(|file| file.path == "new").unwrap();
        assert_eq!(old.status, "D");
        assert_eq!(new.status, "A");
        let old_delta = old.gitlink.as_ref().unwrap();
        let new_delta = new.gitlink.as_ref().unwrap();
        assert_side(&old_delta.old, "gitlink", pointer, 0o160000);
        assert!(old_delta.new.is_none());
        assert!(new_delta.old.is_none());
        assert_side(&new_delta.new, "gitlink", pointer, 0o160000);
    }

    #[test]
    fn gitlink_type_changes_keep_the_real_file_side() {
        let (_temp, repo, pointer) = create_repo_with_file("base", "content");
        let pointer = Oid::from_str(&pointer).unwrap();
        let blob = repo.blob(b"regular content\n").unwrap();
        let file = committed_tree(&repo, None, &[("child", blob, 0o100644)]);
        let link = committed_tree(&repo, Some(file), &[("child", pointer, 0o160000)]);
        let changed = single_detailed(&repo, link);
        assert_eq!(changed.status, "M");
        let delta = changed.gitlink.unwrap();
        assert_side(&delta.old, "file", blob, 0o100644);
        assert_side(&delta.new, "gitlink", pointer, 0o160000);
        let back = committed_tree(&repo, Some(link), &[("child", blob, 0o100644)]);
        let delta = single_detailed(&repo, back).gitlink.unwrap();
        assert_side(&delta.old, "gitlink", pointer, 0o160000);
        assert_side(&delta.new, "file", blob, 0o100644);
    }

    #[test]
    fn gitlink_replaced_by_directory_reports_tree_mode() {
        let (_temp, repo, pointer) = create_repo_with_file("base", "content");
        let pointer = Oid::from_str(&pointer).unwrap();
        let blob = repo.blob(b"content").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file", blob, 0o100644).unwrap();
        let tree = builder.write().unwrap();
        let before = committed_tree(&repo, None, &[("child", pointer, 0o160000)]);
        let after = committed_tree(&repo, Some(before), &[("child", tree, 0o040000)]);
        let (_, _, files) =
            list_commit_files_detailed(repo.workdir().unwrap(), &after.to_string()).unwrap();
        let file = files.into_iter().find(|file| file.path == "child").unwrap();
        let delta = file.gitlink.unwrap();
        assert_side(&delta.old, "gitlink", pointer, 0o160000);
        assert_side(&delta.new, "tree", tree, 0o040000);
    }

    #[test]
    fn detailed_commit_files_reject_missing_and_invalid_commit_objects() {
        let (temp, _, _) = create_repo_with_file("base", "content");
        assert!(list_commit_files_detailed(temp.path(), "not-an-object").is_err());
        assert!(list_commit_files_detailed(temp.path(), &"1".repeat(40)).is_err());
    }

    fn create_repo_with_file(file_name: &str, content: &str) -> (TempDir, Repository, String) {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        fs::write(tmp.path().join(file_name), content).unwrap();
        let oid = {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new(file_name)).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = Signature::now("Test", "test@test.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "Add file", &tree, &[])
                .unwrap()
        };

        (tmp, repo, oid.to_string())
    }

    #[test]
    fn test_list_commit_files_on_known_commit() {
        let (tmp, repo, first_oid) = create_repo_with_file("hello.txt", "hello");

        // Second commit with a modification
        fs::write(tmp.path().join("hello.txt"), "world").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let parent = repo
            .find_commit(git2::Oid::from_str(&first_oid).unwrap())
            .unwrap();
        let oid2 = repo
            .commit(Some("HEAD"), &sig, &sig, "Modify file", &tree, &[&parent])
            .unwrap();

        let (message, files) = list_commit_files(tmp.path(), &oid2.to_string()).unwrap();
        assert_eq!(message, "Modify file");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "M");
        assert_eq!(files[0].1, "hello.txt");
    }

    #[test]
    fn test_root_commit_lists_files() {
        let (tmp, _repo, oid) = create_repo_with_file("root.txt", "content");

        let (message, files) = list_commit_files(tmp.path(), &oid).unwrap();
        assert_eq!(message, "Add file");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "A");
        assert_eq!(files[0].1, "root.txt");
    }

    #[test]
    fn test_batch_diff_stats_returns_additions() {
        let (tmp, repo, first_oid) = create_repo_with_file("file.txt", "line1\n");

        // Second commit adds lines
        fs::write(tmp.path().join("file.txt"), "line1\nline2\nline3\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let parent = repo
            .find_commit(Oid::from_str(&first_oid).unwrap())
            .unwrap();
        let oid2 = repo
            .commit(Some("HEAD"), &sig, &sig, "Add lines", &tree, &[&parent])
            .unwrap();

        let stats = batch_diff_stats(tmp.path(), &[oid2]).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, oid2);
        assert_eq!(stats[0].1.additions, 2);
        assert_eq!(stats[0].1.deletions, 0);
    }

    #[test]
    fn test_batch_diff_stats_root_commit() {
        let (tmp, _repo, oid_str) = create_repo_with_file("root.txt", "content\n");
        let oid = Oid::from_str(&oid_str).unwrap();

        let stats = batch_diff_stats(tmp.path(), &[oid]).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, oid);
        assert_eq!(stats[0].1.additions, 1);
        assert_eq!(stats[0].1.deletions, 0);
    }
}
