use super::*;
use git2::Repository;

fn git(path: &Path, args: &[&str]) {
    let out = crate::git::process::git_command(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(names: &[&str]) -> (tempfile::TempDir, Repository) {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let mut index = repo.index().unwrap();
    for name in names {
        std::fs::write(tmp.path().join(name), format!("original {name}\n")).unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    (tmp, repo)
}

fn apply(path: &Path, name: &str, operation: FileOperation) {
    let args = arguments(path, Path::new(name), operation).unwrap();
    let out = crate::git::process::git_command(path)
        .args(&args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn add_conflict(repo: &Repository, path: &Path) {
    let mut index = repo.index().unwrap();
    let mut entry = index.get_path(path, 0).unwrap();
    index.remove_path(path).unwrap();
    for (stage, content) in [
        (1, b"base version\n".as_slice()),
        (2, b"current version\n".as_slice()),
        (3, b"incoming version\n".as_slice()),
    ] {
        entry.flags = (entry.flags & !0x3000) | (stage << 12);
        entry.id = repo.blob(content).unwrap();
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
}

#[test]
fn conflict_diff_labels_index_sides_even_when_the_working_file_is_missing() {
    let (tmp, repo) = fixture(&["conflict.txt"]);
    let path = Path::new("conflict.txt");
    add_conflict(&repo, path);
    std::fs::remove_file(tmp.path().join(path)).unwrap();

    let preview = diff(tmp.path(), path).unwrap();
    assert!(preview.contains("- Ours (Current): "), "{preview}");
    assert!(preview.contains("+ Theirs (Incoming): "), "{preview}");
    assert!(preview.contains("Base: "), "{preview}");
    assert!(preview.contains("\n-current version\n"), "{preview}");
    assert!(preview.contains("\n+incoming version\n"), "{preview}");

    let mut index = repo.index().unwrap();
    index.remove(path, 2).unwrap();
    index.write().unwrap();
    let preview = diff(tmp.path(), path).unwrap();
    assert!(preview.contains("Ours (Current): deleted"), "{preview}");
    assert!(preview.contains("\n+incoming version\n"), "{preview}");
}

#[cfg(unix)]
#[test]
fn configured_merge_tool_resolves_only_the_selected_conflict() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["with spaces.txt", "*.txt", "other.txt"]);
    let selected = Path::new("with spaces.txt");
    let pattern = Path::new("*.txt");
    let other = Path::new("other.txt");
    add_conflict(&repo, selected);
    add_conflict(&repo, other);
    add_conflict(&repo, pattern);
    let mut config = repo.config().unwrap();
    config.set_str("merge.tool", "fixture").unwrap();
    config
        .set_str("mergetool.fixture.cmd", "cat \"$REMOTE\" > \"$MERGED\"")
        .unwrap();
    config
        .set_bool("mergetool.fixture.trustExitCode", true)
        .unwrap();
    config.set_bool("mergetool.prompt", false).unwrap();
    config.set_bool("mergetool.keepBackup", false).unwrap();

    assert!(merge_tool(tmp.path(), pattern).is_err());
    let output = merge_tool(tmp.path(), selected).unwrap().output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(selected)).unwrap(),
        "incoming version\n"
    );
    let fresh = Repository::open(tmp.path()).unwrap().index().unwrap();
    assert!(fresh.get_path(selected, 0).is_some());
    assert!(fresh.get_path(other, 2).is_some());
    assert!(fresh.get_path(pattern, 2).is_some());
    assert!(merge_tool(tmp.path(), selected).is_err());
}

#[cfg(unix)]
#[test]
fn selected_pattern_filename_does_not_change_other_files_or_index_entries() {
    if !crate::git::git_test_available() {
        return;
    }
    for selected in ["*.txt", ":(glob)*.txt", "[ab].txt"] {
        let (tmp, repo) = fixture(&[selected, "a.txt"]);
        for name in [selected, "a.txt"] {
            std::fs::write(tmp.path().join(name), format!("changed {name}\n")).unwrap();
        }
        apply(tmp.path(), selected, FileOperation::Stage);
        assert!(
            repo.status_file(Path::new(selected))
                .unwrap()
                .is_index_modified()
        );
        assert!(
            !repo
                .status_file(Path::new("a.txt"))
                .unwrap()
                .is_index_modified()
        );
        git(tmp.path(), &["add", "-A"]);
        apply(tmp.path(), selected, FileOperation::Unstage);
        assert!(
            !repo
                .status_file(Path::new(selected))
                .unwrap()
                .is_index_modified()
        );
        assert!(
            repo.status_file(Path::new("a.txt"))
                .unwrap()
                .is_index_modified()
        );
        apply(tmp.path(), selected, FileOperation::Discard);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(selected)).unwrap(),
            format!("original {selected}\n")
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "changed a.txt\n"
        );
        assert!(
            repo.status_file(Path::new("a.txt"))
                .unwrap()
                .is_index_modified()
        );
    }
}

#[cfg(unix)]
#[test]
fn deleting_untracked_pattern_filename_preserves_other_files() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, _) = fixture(&["tracked"]);
    for name in ["*.draft", "important.draft"] {
        std::fs::write(tmp.path().join(name), "draft").unwrap();
    }
    apply(tmp.path(), "*.draft", FileOperation::DeleteUntracked);
    assert!(!tmp.path().join("*.draft").exists());
    assert!(tmp.path().join("important.draft").exists());
}

#[test]
fn rename_actions_update_both_paths_and_preserve_other_staged_changes() {
    if !crate::git::git_test_available() {
        return;
    }
    for operation in [
        FileOperation::Stage,
        FileOperation::Unstage,
        FileOperation::Discard,
    ] {
        let (tmp, repo) = fixture(&["old", "other"]);
        git(tmp.path(), &["mv", "old", "new"]);
        std::fs::write(tmp.path().join("new"), "edited after rename\n").unwrap();
        std::fs::write(tmp.path().join("other"), "unrelated staged\n").unwrap();
        git(tmp.path(), &["add", "other"]);
        apply(tmp.path(), "new", operation);
        let index = repo.index().unwrap();
        assert!(
            repo.status_file(Path::new("other"))
                .unwrap()
                .is_index_modified()
        );
        match operation {
            FileOperation::Stage => {
                assert!(index.get_path(Path::new("old"), 0).is_none());
                assert!(!repo.status_file(Path::new("new")).unwrap().is_wt_modified());
            }
            FileOperation::Unstage => {
                assert!(index.get_path(Path::new("old"), 0).is_some());
                assert!(index.get_path(Path::new("new"), 0).is_none());
                assert_eq!(
                    std::fs::read_to_string(tmp.path().join("new")).unwrap(),
                    "edited after rename\n"
                );
            }
            FileOperation::Discard => {
                assert!(index.get_path(Path::new("old"), 0).is_some());
                assert!(index.get_path(Path::new("new"), 0).is_none());
                assert!(!tmp.path().join("new").exists());
                assert_eq!(
                    std::fs::read_to_string(tmp.path().join("old")).unwrap(),
                    "original old\n"
                );
            }
            FileOperation::DeleteUntracked | FileOperation::Diff => unreachable!(),
        }
    }
}

#[cfg(unix)]
#[test]
fn selected_diff_excludes_other_pattern_matches_in_worktree_and_commit() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["*.txt", "other.txt"]);
    for (name, text) in [("*.txt", "SELECTED"), ("other.txt", "UNRELATED")] {
        std::fs::write(tmp.path().join(name), text).unwrap();
    }
    let text = diff(tmp.path(), Path::new("*.txt")).unwrap();
    assert!(text.contains("SELECTED"));
    assert!(!text.contains("UNRELATED"));
    let head = repo.head().unwrap().target().unwrap().to_string();
    let text = crate::git::commit_files::commit_file_diff(tmp.path(), &head, "*.txt").unwrap();
    assert!(text.contains("original *.txt"));
    assert!(!text.contains("original other.txt"));
}

#[test]
fn untracked_diff_reads_from_selected_repository() {
    let (tmp, _) = fixture(&["tracked"]);
    std::fs::write(tmp.path().join("untracked"), "NEW CONTENT").unwrap();
    assert!(
        diff(tmp.path(), Path::new("untracked"))
            .unwrap()
            .contains("NEW CONTENT")
    );
}

#[test]
fn untracked_directory_diff_includes_nested_files_and_preserves_index() {
    let (tmp, repo) = fixture(&["tracked"]);
    std::fs::create_dir_all(tmp.path().join("new/nested")).unwrap();
    std::fs::write(tmp.path().join("new/first"), "FIRST CONTENT\n").unwrap();
    std::fs::write(tmp.path().join("new/nested/second"), "NESTED CONTENT\n").unwrap();
    std::fs::write(tmp.path().join("unrelated"), "EXCLUDED CONTENT\n").unwrap();
    let index_before = std::fs::read(repo.path().join("index")).unwrap();
    let text = diff(tmp.path(), Path::new("new/")).unwrap();
    assert!(text.contains("FIRST CONTENT"));
    assert!(text.contains("NESTED CONTENT"));
    assert!(!text.contains("EXCLUDED CONTENT"));
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        index_before
    );
}

#[cfg(unix)]
#[test]
fn untracked_pattern_directory_diff_is_literal_and_does_not_follow_symlinks() {
    let (tmp, _) = fixture(&["tracked"]);
    std::fs::create_dir(tmp.path().join("*.draft")).unwrap();
    std::fs::create_dir(tmp.path().join("other.draft")).unwrap();
    std::fs::write(tmp.path().join("*.draft/file"), "SELECTED CONTENT\n").unwrap();
    std::fs::write(tmp.path().join("other.draft/file"), "UNRELATED CONTENT\n").unwrap();
    std::os::unix::fs::symlink("../other.draft/file", tmp.path().join("*.draft/link")).unwrap();
    let text = diff(tmp.path(), Path::new("*.draft/")).unwrap();
    assert!(text.contains("SELECTED CONTENT"));
    assert!(text.contains("../other.draft/file"));
    assert!(!text.contains("UNRELATED CONTENT"));
}

#[test]
fn untracked_binary_diff_reports_binary_content() {
    let (tmp, _) = fixture(&["tracked"]);
    std::fs::write(tmp.path().join("binary"), b"\0\x01\x02").unwrap();
    let text = diff(tmp.path(), Path::new("binary")).unwrap();
    assert!(text.contains("binary"));
    assert!(text.contains("Binary"));
}

#[test]
fn tracked_diff_includes_staged_and_unstaged_content_only_for_selected_file() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["selected", "other"]);
    std::fs::write(tmp.path().join("selected"), "STAGED CONTENT\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("selected")).unwrap();
    index.write().unwrap();
    std::fs::write(
        tmp.path().join("selected"),
        "STAGED CONTENT\nUNSTAGED CONTENT\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("other"), "UNRELATED CONTENT\n").unwrap();
    let text = diff(tmp.path(), Path::new("selected")).unwrap();
    assert!(text.contains("STAGED CONTENT"));
    assert!(text.contains("UNSTAGED CONTENT"));
    assert!(!text.contains("UNRELATED CONTENT"));
}

#[test]
fn discarding_staged_addition_before_first_commit_preserves_other_changes() {
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();
    for name in ["selected", "other"] {
        std::fs::write(tmp.path().join(name), format!("{name} content\n")).unwrap();
    }
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("selected")).unwrap();
    index.add_path(Path::new("other")).unwrap();
    index.write().unwrap();
    std::fs::write(tmp.path().join("untracked"), "untracked content\n").unwrap();
    std::fs::write(tmp.path().join("selected"), "edited staged addition\n").unwrap();
    apply(tmp.path(), "selected", FileOperation::Discard);
    let repo = Repository::open(tmp.path()).unwrap();
    assert!(!tmp.path().join("selected").exists());
    assert!(
        repo.index()
            .unwrap()
            .get_path(Path::new("selected"), 0)
            .is_none()
    );
    assert!(repo.status_file(Path::new("other")).unwrap().is_index_new());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("other")).unwrap(),
        "other content\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("untracked")).unwrap(),
        "untracked content\n"
    );
}

#[test]
fn unborn_discard_refuses_untracked_paths_and_directories() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();
    std::fs::create_dir(tmp.path().join("directory")).unwrap();
    std::fs::write(tmp.path().join("directory/staged"), "staged content").unwrap();
    std::fs::write(tmp.path().join("untracked"), "untracked content").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("directory/staged")).unwrap();
    index.write().unwrap();
    let index_before = std::fs::read(repo.path().join("index")).unwrap();
    for selected in ["untracked", "directory/"] {
        assert!(arguments(tmp.path(), Path::new(selected), FileOperation::Discard).is_err());
    }
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        index_before
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("directory/staged")).unwrap(),
        "staged content"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("untracked")).unwrap(),
        "untracked content"
    );
}

#[test]
fn staging_rename_does_not_stage_a_recreated_source_file() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["old"]);
    git(tmp.path(), &["mv", "old", "new"]);
    std::fs::write(tmp.path().join("old"), "unrelated replacement").unwrap();
    std::fs::write(tmp.path().join("new"), "updated destination").unwrap();
    apply(tmp.path(), "new", FileOperation::Stage);
    assert!(repo.status_file(Path::new("old")).unwrap().is_wt_new());
    assert!(!repo.status_file(Path::new("new")).unwrap().is_wt_modified());
}

#[test]
fn discarding_rename_refuses_to_overwrite_a_recreated_source() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["old"]);
    git(tmp.path(), &["mv", "old", "new"]);
    std::fs::write(tmp.path().join("old"), "unrelated replacement").unwrap();
    let index_before = std::fs::read(repo.path().join("index")).unwrap();
    let error = arguments(tmp.path(), Path::new("new"), FileOperation::Discard).unwrap_err();
    assert!(error.to_string().contains("old"));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("old")).unwrap(),
        "unrelated replacement"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("new")).unwrap(),
        "original old\n"
    );
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        index_before
    );
}

#[cfg(unix)]
#[test]
fn discarding_rename_preserves_a_recreated_dangling_symlink() {
    if !crate::git::git_test_available() {
        return;
    }
    let (tmp, repo) = fixture(&["old"]);
    git(tmp.path(), &["mv", "old", "new"]);
    std::os::unix::fs::symlink("missing-target", tmp.path().join("old")).unwrap();
    let index_before = std::fs::read(repo.path().join("index")).unwrap();
    assert!(arguments(tmp.path(), Path::new("new"), FileOperation::Discard).is_err());
    assert_eq!(
        std::fs::read_link(tmp.path().join("old")).unwrap(),
        Path::new("missing-target")
    );
    assert!(tmp.path().join("new").exists());
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        index_before
    );
}
