use git2::{Diff, DiffOptions, Oid, Repository};
use std::path::Path;

/// List files changed in a commit (vs its first parent, or empty tree for root).
/// Returns `(status_label, file_path)` pairs.
pub(crate) fn list_commit_files(
    path: &Path,
    oid_str: &str,
) -> color_eyre::Result<Vec<(String, String)>> {
    let repo = Repository::open(path)?;
    let oid = Oid::from_str(oid_str)?;
    let commit = repo.find_commit(oid)?;

    let tree = commit.tree()?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut files = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "A",
            git2::Delta::Deleted => "D",
            git2::Delta::Modified => "M",
            git2::Delta::Renamed => "R",
            _ => "?",
        };
        let file_path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        files.push((status.to_string(), file_path));
    }

    Ok(files)
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
    opts.pathspec(file_path);

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
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
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

fn diff_to_string(diff: &Diff<'_>, output: &mut String) -> color_eyre::Result<()> {
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

        let files = list_commit_files(tmp.path(), &oid2.to_string()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "M");
        assert_eq!(files[0].1, "hello.txt");
    }

    #[test]
    fn test_root_commit_lists_files() {
        let (tmp, _repo, oid) = create_repo_with_file("root.txt", "content");

        let files = list_commit_files(tmp.path(), &oid).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "A");
        assert_eq!(files[0].1, "root.txt");
    }
}
