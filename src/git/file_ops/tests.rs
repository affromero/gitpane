use super::*;
use git2::Repository;

fn git(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
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
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
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

#[cfg(unix)]
#[test]
fn selected_pattern_filename_does_not_change_other_files_or_index_entries() {
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
fn staging_rename_does_not_stage_a_recreated_source_file() {
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
