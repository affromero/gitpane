use std::fs;
use std::path::Path;

use git2::Repository;
use tempfile::TempDir;

use super::{StashContentKind, StashSource, list_stash_files, stash_file_content};

fn git(path: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = crate::git::process::git_command(path)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn oid(path: &Path, reference: &str) -> String {
    String::from_utf8(git(path, &["rev-parse", "--verify", reference]))
        .unwrap()
        .trim()
        .to_string()
}

fn repository() -> TempDir {
    let directory = TempDir::new().unwrap();
    Repository::init(directory.path()).unwrap();
    fs::write(directory.path().join("tracked.txt"), b"base\n").unwrap();
    fs::write(directory.path().join("deleted.txt"), b"deleted contents\n").unwrap();
    fs::write(directory.path().join("old.txt"), b"rename this snapshot\n").unwrap();
    fs::write(directory.path().join(".gitignore"), b"ignored.txt\n").unwrap();
    git(directory.path(), &["add", "--", "."]);
    git(directory.path(), &["commit", "-m", "base"]);
    directory
}

#[test]
fn real_all_stash_combines_changes_and_remains_immutable_after_renumbering() {
    if !crate::git::git_test_available() {
        return;
    }
    let directory = repository();
    let path = directory.path();
    fs::write(path.join("tracked.txt"), b"staged\n").unwrap();
    git(path, &["add", "--", "tracked.txt"]);
    fs::write(path.join("tracked.txt"), b"saved final unstaged\n").unwrap();
    fs::remove_file(path.join("deleted.txt")).unwrap();
    fs::rename(path.join("old.txt"), path.join("renamed.txt")).unwrap();
    git(
        path,
        &["add", "--", "old.txt", "renamed.txt", "deleted.txt"],
    );
    fs::write(path.join("untracked.txt"), b"saved untracked\n").unwrap();
    fs::write(path.join("ignored.txt"), b"saved ignored\n").unwrap();
    git(path, &["stash", "push", "--all", "-m", "first snapshot"]);
    let original = oid(path, "refs/stash");
    let listing = list_stash_files(path, &original).unwrap();
    assert!(listing.files.iter().any(|file| file.status == "R"
        && file.old_path.as_deref() == Some("old.txt")
        && file.path == "renamed.txt"));
    assert!(
        listing
            .files
            .iter()
            .any(|file| file.status == "D" && file.path == "deleted.txt")
    );
    let original_content =
        stash_file_content(path, &original, StashSource::Tracked, "tracked.txt").unwrap();
    assert_eq!(original_content.old.text.as_deref(), Some("base\n"));
    assert_eq!(
        original_content.new.text.as_deref(),
        Some("saved final unstaged\n")
    );
    for (name, expected) in [
        ("untracked.txt", "saved untracked\n"),
        ("ignored.txt", "saved ignored\n"),
    ] {
        let content =
            stash_file_content(path, &original, StashSource::SavedUntracked, name).unwrap();
        assert_eq!(content.old.kind, StashContentKind::Missing);
        assert_eq!(content.new.text.as_deref(), Some(expected));
    }

    fs::write(path.join("tracked.txt"), b"later HEAD\n").unwrap();
    git(path, &["add", "--", "tracked.txt"]);
    git(path, &["commit", "-m", "advance HEAD"]);
    fs::write(path.join("tracked.txt"), b"later stash\n").unwrap();
    git(path, &["stash", "push", "-m", "second snapshot"]);
    assert_eq!(oid(path, "stash@{1}"), original);
    fs::write(path.join("tracked.txt"), b"unrelated current index\n").unwrap();
    git(path, &["add", "--", "tracked.txt"]);
    fs::write(path.join("tracked.txt"), b"unrelated current worktree\n").unwrap();
    fs::write(path.join("untracked.txt"), b"unrelated current untracked\n").unwrap();
    let before = (
        git(
            path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        ),
        fs::read(path.join(".git/index")).unwrap(),
        fs::read(path.join("tracked.txt")).unwrap(),
        fs::read(path.join("untracked.txt")).unwrap(),
        oid(path, "HEAD"),
        git(path, &["reflog", "show", "refs/stash", "--format=%H %gs"]),
    );
    assert_eq!(list_stash_files(path, &original).unwrap(), listing);
    assert_eq!(
        stash_file_content(path, &original, StashSource::Tracked, "tracked.txt").unwrap(),
        original_content
    );
    let after = (
        git(
            path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        ),
        fs::read(path.join(".git/index")).unwrap(),
        fs::read(path.join("tracked.txt")).unwrap(),
        fs::read(path.join("untracked.txt")).unwrap(),
        oid(path, "HEAD"),
        git(path, &["reflog", "show", "refs/stash", "--format=%H %gs"]),
    );
    assert_eq!(before, after);
}

#[test]
fn real_ordinary_stash_has_no_saved_untracked_parent() {
    if !crate::git::git_test_available() {
        return;
    }
    let directory = repository();
    let path = directory.path();
    fs::write(path.join("tracked.txt"), b"tracked stash\n").unwrap();
    fs::write(path.join("untracked.txt"), b"left in worktree\n").unwrap();
    git(path, &["stash", "push", "-m", "ordinary stash"]);
    let listing = list_stash_files(path, &oid(path, "refs/stash")).unwrap();
    assert_eq!(listing.untracked_parent_oid, None);
    assert_eq!(listing.files.len(), 1);
    assert_eq!(listing.files[0].source, StashSource::Tracked);
    assert_eq!(listing.files[0].path, "tracked.txt");
    assert_eq!(
        fs::read(path.join("untracked.txt")).unwrap(),
        b"left in worktree\n"
    );
}

#[test]
fn real_untracked_only_stashes_include_ignored_files_only_with_all() {
    if !crate::git::git_test_available() {
        return;
    }
    for option in ["--include-untracked", "--all"] {
        let directory = repository();
        let path = directory.path();
        fs::write(path.join("untracked.txt"), b"untracked only\n").unwrap();
        fs::write(path.join("ignored.txt"), b"ignored only\n").unwrap();
        git(path, &["stash", "push", option, "-m", "untracked only"]);
        let original = oid(path, "refs/stash");
        let listing = list_stash_files(path, &original).unwrap();
        assert!(listing.untracked_parent_oid.is_some());
        assert_eq!(listing.files.len(), if option == "--all" { 2 } else { 1 });
        assert!(
            listing
                .files
                .iter()
                .all(|file| file.source == StashSource::SavedUntracked && file.status == "A")
        );
        let content = stash_file_content(
            path,
            &original,
            StashSource::SavedUntracked,
            "untracked.txt",
        )
        .unwrap();
        assert_eq!(content.old.kind, StashContentKind::Missing);
        assert_eq!(content.new.text.as_deref(), Some("untracked only\n"));
    }
}
