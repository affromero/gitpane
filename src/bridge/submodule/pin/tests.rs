use super::*;
use crate::bridge::submodule::tests::{commit_file, fixture};
use git2::Repository;
use std::fs;

fn link(scope: &Scope, parent: &Path, path: &str) -> Link {
    let current = registered_link(scope, parent, Path::new(path), true).unwrap();
    Link {
        path: path.into(),
        parent_identity: current.parent_identity.fingerprint().unwrap(),
        child_identity: current.child_identity.fingerprint().unwrap(),
    }
}

fn request(root: &Path, chain: Vec<Link>) -> Request {
    Request {
        root: root.to_path_buf(),
        chain,
    }
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
fn durable_nested_pin_survives_commits_and_working_changes() {
    let (temp, parent, child, scope) = fixture();
    let child_repo = Repository::open(&child).unwrap();
    let mut module = child_repo
        .submodule(
            temp.path().join("source").to_str().unwrap(),
            Path::new("nested"),
            true,
        )
        .unwrap();
    module.clone(None).unwrap();
    module.add_finalize().unwrap();
    drop(module);
    let links = vec![
        link(&scope, &parent, "child"),
        link(&scope, &child, "nested"),
    ];
    let input = request(&parent, links);
    let expected = vec![
        parent.canonicalize().unwrap(),
        child.canonicalize().unwrap(),
        child.join("nested").canonicalize().unwrap(),
    ];
    assert_eq!(
        validate(&scope, input.clone()).unwrap(),
        json!({"repos": expected})
    );
    commit_file(&child_repo, "new", "new commit");
    fs::write(child.join("nested/base"), "modified").unwrap();
    let nested_repo = Repository::open(child.join("nested")).unwrap();
    commit_file(&nested_repo, "next", "nested commit");
    assert_eq!(validate(&scope, input).unwrap(), json!({"repos": expected}));
}

#[test]
fn pin_rejects_repointed_gitdir_even_when_head_is_unchanged() {
    let (_temp, parent, child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    let repo = Repository::open(&child).unwrap();
    let head = repo.head().unwrap().target().unwrap();
    let original = repo.path().canonicalize().unwrap();
    drop(repo);
    let replacement = original.parent().unwrap().join("replacement-pin");
    copy_directory(&original, &replacement);
    fs::write(
        child.join(".git"),
        format!("gitdir: {}\n", replacement.display()),
    )
    .unwrap();
    assert_eq!(
        Repository::open(&child).unwrap().head().unwrap().target(),
        Some(head)
    );
    assert_eq!(validate(&scope, input).unwrap_err().code, "stale_target");
}

#[test]
fn rejects_invalid_paths_tokens_and_chain_limits() {
    let (_temp, parent, _child, scope) = fixture();
    let good = link(&scope, &parent, "child");
    assert_eq!(
        validate(&scope, request(&parent, vec![])).unwrap_err().code,
        "invalid_request"
    );
    assert_eq!(
        validate(&scope, request(&parent, vec![good.clone(); 33]))
            .unwrap_err()
            .code,
        "chain_too_large"
    );
    for path in ["", "../outside", "/absolute", "child/../child"] {
        let mut invalid = good.clone();
        invalid.path = path.into();
        assert_eq!(
            validate(&scope, request(&parent, vec![invalid]))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    let mut invalid = good;
    invalid.child_identity = "invalid".to_owned();
    assert_eq!(
        validate(&scope, request(&parent, vec![invalid]))
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

#[test]
fn missing_or_uninitialized_child_cannot_be_used() {
    let (_temp, parent, child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    fs::rename(&child, parent.join("saved-child")).unwrap();
    let error = validate(&scope, input).unwrap_err();
    assert_eq!(error.code, "repository_error");
    assert!(error.message.contains("not initialized"));
}

#[test]
fn removed_registration_invalidates_pin_even_if_gitlink_and_child_remain() {
    let (_temp, parent, child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    fs::write(parent.join(".gitmodules"), "").unwrap();
    assert!(child.join(".git").exists());
    assert_eq!(validate(&scope, input).unwrap_err().code, "stale_target");
}

#[test]
fn configured_submodule_requires_a_head_or_index_gitlink() {
    let (_temp, parent, child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    let repo = Repository::open(&parent).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("child")).unwrap();
    index.write().unwrap();
    assert!(
        validate(&scope, input.clone()).is_ok(),
        "HEAD still contains the gitlink"
    );
    drop(index);
    commit_file(&repo, "parent", "gitlink removed");
    assert!(child.join(".git").exists());
    assert!(parent.join(".gitmodules").exists());
    assert_eq!(validate(&scope, input).unwrap_err().code, "stale_target");
}

#[cfg(unix)]
#[test]
fn submodule_symlinks_cannot_escape_or_form_cycles() {
    use std::os::unix::fs::symlink;
    let (temp, parent, child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    fs::rename(&child, parent.join("saved-child")).unwrap();
    for target in [temp.path().join("source"), parent.clone()] {
        symlink(&target, &child).unwrap();
        assert_eq!(
            validate(&scope, input.clone()).unwrap_err().code,
            "outside_workspace"
        );
        fs::remove_file(&child).unwrap();
    }
}

#[test]
fn workspace_replacement_invalidates_pin() {
    let (temp, parent, _child, scope) = fixture();
    let input = request(&parent, vec![link(&scope, &parent, "child")]);
    let workspace = temp.path().join("workspace");
    fs::rename(&workspace, temp.path().join("original-workspace")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let error = validate(&scope, input).unwrap_err();
    assert!(matches!(
        error.code,
        "outside_workspace" | "repository_error"
    ));
}
