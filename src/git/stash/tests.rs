use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository};
use tempfile::TempDir;

use super::test_support::{commit, files, stash, tree};
use super::{StashContentKind, StashSource, list_stash_files, stash_file_content, stash_file_diff};

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    (directory, repo)
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_do_not_hide_other_saved_files() {
    let (directory, repo) = repository();
    let empty = files(&repo, &[]);
    let base = commit(&repo, empty, &[], "base");
    let blob = repo.blob(b"saved\n").unwrap();
    let mut index = git2::Index::new().unwrap();
    for name in [b"bad-\xff.txt".as_slice(), b"good.txt"] {
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: 0,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: name.to_vec(),
            })
            .unwrap();
    }
    let saved = index.write_tree_to(&repo).unwrap();
    let oid = stash(&repo, base, empty, empty, Some(saved)).to_string();
    let list = list_stash_files(directory.path(), &oid).unwrap();
    assert_eq!(list.files.len(), 2);
    let raw = list
        .files
        .iter()
        .find(|file| file.path.starts_with("~gitpane-raw~"))
        .unwrap();
    let read = stash_file_content(directory.path(), &oid, raw.source, &raw.path).unwrap();
    assert_eq!(read.new.text.as_deref(), Some("saved\n"));
    assert!(list.files.iter().any(|file| file.path == "good.txt"));
}

#[test]
fn two_parent_stash_compares_first_parent_with_combined_saved_tree() {
    let (directory, repo) = repository();
    let base_tree = files(
        &repo,
        &[("combined.txt", b"base\n"), ("deleted.txt", b"removed\n")],
    );
    let base = commit(&repo, base_tree, &[], "base");
    let index = files(&repo, &[("combined.txt", b"staged\n")]);
    let saved = files(
        &repo,
        &[
            ("combined.txt", b"final unstaged\n"),
            ("added.txt", b"brand new\n"),
        ],
    );
    let oid = stash(&repo, base, index, saved, None).to_string();
    let listing = list_stash_files(directory.path(), &oid).unwrap();
    assert_eq!(listing.oid, oid);
    assert_eq!(listing.parent_oid, base.to_string());
    assert_eq!(listing.untracked_parent_oid, None);
    assert_eq!(listing.files.len(), 3);
    assert!(
        listing
            .files
            .iter()
            .all(|file| file.source == StashSource::Tracked)
    );

    let read =
        |path| stash_file_content(directory.path(), &oid, StashSource::Tracked, path).unwrap();
    let combined = read("combined.txt");
    assert_eq!(combined.file.status, "M");
    assert_eq!(combined.old.text.as_deref(), Some("base\n"));
    assert_eq!(combined.new.text.as_deref(), Some("final unstaged\n"));
    assert!(combined.diff.contains("-base\n"));
    assert!(combined.diff.contains("+final unstaged\n"));
    assert!(!combined.diff.contains("+staged\n"));
    assert_eq!(
        stash_file_diff(directory.path(), &oid, StashSource::Tracked, "combined.txt").unwrap(),
        combined.diff
    );
    let added = read("added.txt");
    assert_eq!(added.file.status, "A");
    assert_eq!(added.file.old_path, None);
    assert_eq!(added.old.kind, StashContentKind::Missing);
    assert_eq!(added.old.text.as_deref(), Some(""));
    assert_eq!(added.old.oid, None);
    assert_eq!(added.old.mode, None);
    let deleted = read("deleted.txt");
    assert_eq!(deleted.file.status, "D");
    assert_eq!(deleted.file.new_path, None);
    assert_eq!(deleted.new.kind, StashContentKind::Missing);
    assert_eq!(deleted.new.text.as_deref(), Some(""));
    assert!(deleted.diff.contains("-removed\n"));
    assert!(
        stash_file_content(
            directory.path(),
            &oid,
            StashSource::SavedUntracked,
            "added.txt"
        )
        .is_err()
    );
}

#[test]
fn third_parent_is_added_from_empty_and_keeps_sources_distinct() {
    let (directory, repo) = repository();
    let empty = files(&repo, &[]);
    let base = commit(&repo, empty, &[], "base");
    let saved = files(&repo, &[("same.txt", b"tracked snapshot\n")]);
    let untracked = files(
        &repo,
        &[
            ("same.txt", b"untracked snapshot\n"),
            ("nested/only.txt", b"only\n"),
        ],
    );
    let oid = stash(&repo, base, empty, saved, Some(untracked)).to_string();
    let listing = list_stash_files(directory.path(), &oid).unwrap();
    assert_eq!(listing.files.len(), 3);
    assert!(listing.untracked_parent_oid.is_some());
    for source in [StashSource::Tracked, StashSource::SavedUntracked] {
        let read = stash_file_content(directory.path(), &oid, source, "same.txt").unwrap();
        assert_eq!(read.file.status, "A");
        assert_eq!(read.old.kind, StashContentKind::Missing);
        let expected = if source == StashSource::Tracked {
            "tracked snapshot\n"
        } else {
            "untracked snapshot\n"
        };
        assert_eq!(read.new.text.as_deref(), Some(expected));
    }
    let only_untracked = stash(&repo, base, empty, empty, Some(untracked)).to_string();
    let listing = list_stash_files(directory.path(), &only_untracked).unwrap();
    assert_eq!(listing.files.len(), 2);
    assert!(
        listing
            .files
            .iter()
            .all(|file| file.source == StashSource::SavedUntracked && file.status == "A")
    );
    assert!(
        list_stash_files(
            directory.path(),
            &stash(&repo, base, empty, empty, None).to_string()
        )
        .unwrap()
        .files
        .is_empty()
    );
}

#[test]
fn rename_preserves_both_literal_paths_and_reads_original_side() {
    let (directory, repo) = repository();
    let contents = b"identical saved contents\nsecond line\n";
    let base_tree = files(&repo, &[("old name.txt", contents)]);
    let base = commit(&repo, base_tree, &[], "base");
    let saved = files(&repo, &[("nested/new name.txt", contents)]);
    let oid = stash(&repo, base, saved, saved, None).to_string();
    let listing = list_stash_files(directory.path(), &oid).unwrap();
    assert_eq!(listing.files.len(), 1);
    let file = &listing.files[0];
    assert_eq!(file.status, "R");
    assert_eq!(file.path, "nested/new name.txt");
    assert_eq!(file.old_path.as_deref(), Some("old name.txt"));
    assert_eq!(file.new_path.as_deref(), Some("nested/new name.txt"));
    let read = stash_file_content(directory.path(), &oid, file.source, &file.path).unwrap();
    assert_eq!(read.old.text, read.new.text);
    assert!(read.diff.contains("rename from old name.txt"));
    assert!(read.diff.contains("rename to nested/new name.txt"));
    assert!(stash_file_content(directory.path(), &oid, file.source, "old name.txt").is_err());
}

#[cfg(unix)]
#[test]
fn pathspec_magic_metacharacters_unicode_newlines_and_backslashes_are_literal() {
    let (directory, repo) = repository();
    let names = [
        ":(glob)*",
        "file[1]*?.txt",
        "-leading.txt",
        "nested/空 格.txt",
        "line\nbreak.txt",
        "back\\slash.txt",
        "quote\"'$`.txt",
    ];
    let base_tree = files(&repo, &[]);
    let base = commit(&repo, base_tree, &[], "base");
    let entries: Vec<_> = names.iter().map(|name| (*name, name.as_bytes())).collect();
    let untracked = files(&repo, &entries);
    let oid = stash(&repo, base, base_tree, base_tree, Some(untracked)).to_string();
    let listing = list_stash_files(directory.path(), &oid).unwrap();
    assert_eq!(listing.files.len(), names.len());
    for name in names {
        let read =
            stash_file_content(directory.path(), &oid, StashSource::SavedUntracked, name).unwrap();
        assert_eq!(read.file.path, name);
        assert_eq!(read.new.text.as_deref(), Some(name));
        assert!(read.diff.contains("diff --git"));
    }
    for invalid in [
        "",
        "../outside",
        "/absolute",
        "nested/../file",
        "nested//file",
        "./file",
        "null\0path",
    ] {
        assert!(
            stash_file_content(directory.path(), &oid, StashSource::SavedUntracked, invalid)
                .is_err()
        );
    }
}

#[test]
fn binary_and_invalid_utf8_have_no_decoded_text_and_keep_binary_diff_semantics() {
    let (directory, repo) = repository();
    let base_tree = files(
        &repo,
        &[("binary.dat", b"old\0bytes"), ("deleted.dat", b"gone\0")],
    );
    let base = commit(&repo, base_tree, &[], "base");
    let saved = files(
        &repo,
        &[
            ("binary.dat", b"new\0bytes"),
            ("invalid.txt", &[0xff, 0xfe]),
            ("added.dat", b"added\0"),
        ],
    );
    let oid = stash(&repo, base, saved, saved, None).to_string();
    for name in ["binary.dat", "deleted.dat", "invalid.txt", "added.dat"] {
        let read = stash_file_content(directory.path(), &oid, StashSource::Tracked, name).unwrap();
        assert!(read.file.is_binary);
        assert!(read.diff.contains("diff --git"));
        assert!(read.diff.contains("Binary files differ"));
        assert!(!read.diff.contains('\u{fffd}'));
        for side in [&read.old, &read.new] {
            if side.kind == StashContentKind::Binary {
                assert_eq!(side.text, None);
            }
        }
    }
    assert!(
        list_stash_files(directory.path(), &oid)
            .unwrap()
            .files
            .iter()
            .all(|file| file.is_binary)
    );

    let unchanged = repo.blob(b"unchanged\0binary").unwrap();
    let base_tree = tree(
        &repo,
        &[
            ("mode.dat", unchanged, 0o100644),
            ("old-binary.dat", unchanged, 0o100644),
        ],
    );
    let base = commit(&repo, base_tree, &[], "binary metadata base");
    let saved = tree(
        &repo,
        &[
            ("mode.dat", unchanged, 0o100755),
            ("renamed-binary.dat", unchanged, 0o100644),
        ],
    );
    let oid = stash(&repo, base, saved, saved, None).to_string();
    for name in ["mode.dat", "renamed-binary.dat"] {
        let read = stash_file_content(directory.path(), &oid, StashSource::Tracked, name).unwrap();
        assert!(read.file.is_binary);
        assert_eq!(read.old.oid, read.new.oid);
        assert_eq!(read.old.text, None);
        assert_eq!(read.new.text, None);
        assert!(read.diff.contains("diff --git"));
        assert!(!read.diff.contains("Binary files differ"));
        if name == "mode.dat" {
            assert_eq!(read.old.mode, Some(0o100644));
            assert_eq!(read.new.mode, Some(0o100755));
            assert!(read.diff.contains("old mode 100644"));
            assert!(read.diff.contains("new mode 100755"));
        } else {
            assert_eq!(read.file.status, "R");
            assert!(read.diff.contains("rename from old-binary.dat"));
            assert!(read.diff.contains("rename to renamed-binary.dat"));
        }
    }
}

#[test]
fn gitlinks_show_saved_object_pointers_without_opening_submodules() {
    let (directory, repo) = repository();
    let old_oid = Oid::from_str(&"1".repeat(40)).unwrap();
    let new_oid = Oid::from_str(&"2".repeat(40)).unwrap();
    let base_tree = tree(&repo, &[("module", old_oid, 0o160000)]);
    let base = commit(&repo, base_tree, &[], "base");
    let saved = tree(&repo, &[("module", new_oid, 0o160000)]);
    let oid = stash(&repo, base, saved, saved, None).to_string();
    let read = stash_file_content(directory.path(), &oid, StashSource::Tracked, "module").unwrap();
    assert!(read.file.is_submodule);
    assert!(!read.file.is_binary);
    assert_eq!(read.old.kind, StashContentKind::Gitlink);
    assert_eq!(read.new.kind, StashContentKind::Gitlink);
    assert_eq!(read.old.mode, Some(0o160000));
    assert_eq!(read.new.oid, Some(new_oid.to_string()));
    assert_eq!(
        read.old.text,
        Some(format!("Subproject commit {old_oid}\n"))
    );
    assert!(read.diff.contains(&format!("-Subproject commit {old_oid}")));
    assert!(read.diff.contains(&format!("+Subproject commit {new_oid}")));
    assert!(!directory.path().join("module").exists());
}

#[test]
fn type_changes_symlinks_and_executable_modes_use_saved_entries() {
    let (directory, repo) = repository();
    let text = repo.blob(b"saved text\n").unwrap();
    let target = repo.blob(b"../../outside.txt").unwrap();
    let pointer = Oid::from_str(&"3".repeat(40)).unwrap();
    let base_tree = tree(
        &repo,
        &[("mode.txt", text, 0o100644), ("changed", text, 0o100644)],
    );
    let base = commit(&repo, base_tree, &[], "base");
    let saved = tree(
        &repo,
        &[
            ("mode.txt", text, 0o100755),
            ("changed", pointer, 0o160000),
            ("link", target, 0o120000),
        ],
    );
    let oid = stash(&repo, base, saved, saved, None).to_string();
    let read = stash_file_content(directory.path(), &oid, StashSource::Tracked, "changed").unwrap();
    assert_eq!(read.file.status, "T");
    assert_eq!(read.old.kind, StashContentKind::Text);
    assert_eq!(read.new.kind, StashContentKind::Gitlink);
    let executable =
        stash_file_content(directory.path(), &oid, StashSource::Tracked, "mode.txt").unwrap();
    assert_eq!(executable.old.mode, Some(0o100644));
    assert_eq!(executable.new.mode, Some(0o100755));
    assert!(executable.diff.contains("new mode 100755"));
    let link = stash_file_content(directory.path(), &oid, StashSource::Tracked, "link").unwrap();
    assert_eq!(link.new.mode, Some(0o120000));
    assert_eq!(link.new.text.as_deref(), Some("../../outside.txt"));
}

fn disk_snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), output);
            } else {
                output.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    visit(path, path, &mut output);
    output
}

#[test]
fn current_head_index_attributes_worktree_and_stash_numbering_cannot_change_snapshot() {
    let (directory, repo) = repository();
    let base_tree = files(&repo, &[("saved.txt", b"original base\n")]);
    let base = commit(&repo, base_tree, &[], "base");
    let saved = files(&repo, &[("saved.txt", b"immutable snapshot\n")]);
    let oid = stash(&repo, base, saved, saved, None);
    repo.reference("refs/stash", oid, true, "original stash")
        .unwrap();
    let expected = stash_file_content(
        directory.path(),
        &oid.to_string(),
        StashSource::Tracked,
        "saved.txt",
    )
    .unwrap();
    let expected_files = list_stash_files(directory.path(), &oid.to_string()).unwrap();

    let later_tree = files(&repo, &[("saved.txt", b"later HEAD\n")]);
    let later = commit(&repo, later_tree, &[base], "later HEAD");
    repo.reference("refs/heads/later", later, true, "advance HEAD")
        .unwrap();
    repo.set_head("refs/heads/later").unwrap();
    let other_stash = stash(&repo, later, later_tree, saved, None);
    repo.reference("refs/stash", other_stash, true, "renumber original stash")
        .unwrap();
    fs::write(
        directory.path().join("saved.txt"),
        b"current unrelated workspace\n",
    )
    .unwrap();
    fs::write(directory.path().join(".gitattributes"), b"* binary\n").unwrap();
    fs::write(repo.path().join("info/attributes"), b"* binary\n").unwrap();
    fs::write(repo.path().join("index"), b"deliberately unreadable index").unwrap();
    let before = disk_snapshot(directory.path());
    assert_eq!(
        list_stash_files(directory.path(), &oid.to_string()).unwrap(),
        expected_files
    );
    assert_eq!(
        stash_file_content(
            directory.path(),
            &oid.to_string(),
            StashSource::Tracked,
            "saved.txt"
        )
        .unwrap(),
        expected
    );
    assert_eq!(disk_snapshot(directory.path()), before);
}

#[test]
fn invalid_short_nonstash_and_missing_objects_return_errors() {
    let (directory, repo) = repository();
    let empty = files(&repo, &[]);
    let base = commit(&repo, empty, &[], "not a stash");
    for oid in [
        "HEAD".to_string(),
        "stash@{0}".to_string(),
        base.to_string()[..12].to_string(),
        "g".repeat(40),
        "0".repeat(40),
        base.to_string(),
        repo.blob(b"not a commit").unwrap().to_string(),
    ] {
        assert!(list_stash_files(directory.path(), &oid).is_err(), "{oid}");
    }
    let saved = files(&repo, &[("missing.txt", b"will disappear\n")]);
    let untracked = commit(&repo, saved, &[], "third parent");
    let index = commit(&repo, empty, &[base], "index");
    let oid = commit(&repo, empty, &[base, index, untracked], "stash");
    let id = untracked.to_string();
    fs::remove_file(repo.path().join("objects").join(&id[..2]).join(&id[2..])).unwrap();
    assert!(list_stash_files(directory.path(), &oid.to_string()).is_err());
}

#[test]
fn linked_worktree_uses_shared_objects_without_current_indexes_or_attributes() {
    let (directory, repo) = repository();
    let base_tree = files(&repo, &[("nested/saved.txt", b"original base\n")]);
    let base = commit(&repo, base_tree, &[], "base");
    let saved = files(&repo, &[("nested/saved.txt", b"immutable snapshot\n")]);
    let oid = stash(&repo, base, saved, saved, None).to_string();
    repo.reference("refs/heads/main", base, true, "base")
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let expected_files = list_stash_files(directory.path(), &oid).unwrap();
    let expected = stash_file_content(
        directory.path(),
        &oid,
        StashSource::Tracked,
        "nested/saved.txt",
    )
    .unwrap();

    let linked_container = TempDir::new().unwrap();
    let linked_path = linked_container.path().join("linked");
    let _worktree = repo.worktree("preview", &linked_path, None).unwrap();
    let linked = Repository::open(&linked_path).unwrap();
    assert!(linked_path.join(".git").is_file());
    assert_eq!(
        linked.commondir().canonicalize().unwrap(),
        repo.commondir().canonicalize().unwrap()
    );
    fs::write(repo.path().join("index"), b"corrupt main index").unwrap();
    fs::write(linked.path().join("index"), b"corrupt linked index").unwrap();
    fs::write(repo.path().join("info/attributes"), b"* binary\n").unwrap();
    fs::write(linked_path.join(".gitattributes"), b"* binary\n").unwrap();
    fs::write(linked_path.join("nested/.gitattributes"), b"* binary\n").unwrap();
    fs::write(
        linked_path.join("nested/saved.txt"),
        b"unrelated checkout\n",
    )
    .unwrap();
    repo.config()
        .unwrap()
        .set_bool("diff.noprefix", true)
        .unwrap();
    let main_before = disk_snapshot(directory.path());
    let linked_before = disk_snapshot(&linked_path);

    assert_eq!(
        list_stash_files(&linked_path, &oid).unwrap(),
        expected_files
    );
    assert_eq!(
        stash_file_content(&linked_path, &oid, StashSource::Tracked, "nested/saved.txt").unwrap(),
        expected
    );
    assert_eq!(
        stash_file_diff(&linked_path, &oid, StashSource::Tracked, "nested/saved.txt").unwrap(),
        expected.diff
    );
    assert_eq!(disk_snapshot(directory.path()), main_before);
    assert_eq!(disk_snapshot(&linked_path), linked_before);
}
