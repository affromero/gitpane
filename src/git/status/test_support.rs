use git2::Repository;
use std::{fs, path::Path};
use tempfile::TempDir;

pub(super) fn commit_index(repo: &Repository, message: &str) -> git2::Oid {
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let mut index = repo.index().unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let head = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit<'_>> = head.iter().collect();

    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap()
}

pub(super) fn init_temp_repo() -> (TempDir, Repository) {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    // Create initial commit so HEAD exists
    commit_index(&repo, "Initial commit");

    (tmp, repo)
}

/// Helper: creates a temp repo with a submodule, returns (parent_tmp, sub_source_tmp, sub_repo)
pub(super) fn init_repo_with_submodule() -> (TempDir, TempDir, Repository) {
    let (tmp, repo) = init_temp_repo();

    let sub_source = TempDir::new().unwrap();
    let sub_repo = Repository::init(sub_source.path()).unwrap();
    {
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        fs::write(sub_source.path().join("lib.rs"), "fn hello() {}").unwrap();
        let mut idx = sub_repo.index().unwrap();
        idx.add_path(Path::new("lib.rs")).unwrap();
        idx.write().unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = sub_repo.find_tree(tree_id).unwrap();
        sub_repo
            .commit(Some("HEAD"), &sig, &sig, "init sub", &tree, &[])
            .unwrap();
    }

    let mut submodule = repo
        .submodule(
            sub_source.path().to_str().unwrap(),
            Path::new("my-sub"),
            true,
        )
        .unwrap();
    submodule.clone(None).unwrap();
    submodule.add_finalize().unwrap();
    commit_index(&repo, "add submodule");

    (tmp, sub_source, sub_repo)
}

pub(super) fn stage_submodule_pointer(parent_path: &Path, sub_dirname: &str) {
    let repo = Repository::open(parent_path).unwrap();
    let mut submodule = repo.find_submodule(sub_dirname).unwrap();
    submodule.reload(true).unwrap();
    submodule.add_to_index(true).unwrap();
}

/// Adds a new commit on top of the submodule's current HEAD without pushing.
/// Updates HEAD (works whether HEAD is detached or on a branch).
/// Returns the new commit oid.
pub(super) fn add_unpushed_commit_in_sub(parent_path: &Path, sub_dirname: &str) -> git2::Oid {
    let sub_repo = Repository::open(parent_path.join(sub_dirname)).unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let head_commit = sub_repo.head().unwrap().peel_to_commit().unwrap();

    fs::write(
        parent_path.join(sub_dirname).join("extra.rs"),
        "fn extra() {}",
    )
    .unwrap();
    let mut idx = sub_repo.index().unwrap();
    idx.add_path(Path::new("extra.rs")).unwrap();
    idx.write().unwrap();
    let tree_id = idx.write_tree().unwrap();
    let tree = sub_repo.find_tree(tree_id).unwrap();

    sub_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "unpushed work",
            &tree,
            &[&head_commit],
        )
        .unwrap()
}

pub(super) fn add_commit_on_head(repo: &Repository, file: &str, contents: &str) -> git2::Oid {
    let workdir = repo.workdir().unwrap().to_path_buf();
    fs::write(workdir.join(file), contents).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(file)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("Add {}", file),
        &tree,
        &[&parent],
    )
    .unwrap()
}
