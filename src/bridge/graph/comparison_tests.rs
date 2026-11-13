use std::path::Path;

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use super::{
    CompareRevisions, ResolveRevision, RevisionRefs, Scope, compare_revisions, resolve_revision,
    revision_refs,
};

fn commit(repo: &Repository, file: Option<&str>, parent: Option<Oid>) -> Oid {
    let mut index = repo.index().unwrap();
    if let Some(content) = file {
        std::fs::write(repo.workdir().unwrap().join("file.txt"), content).unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
    }
    let tree_id = index.write_tree().unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let parents = parent.map(|id| repo.find_commit(id).unwrap());
    let parents: Vec<_> = parents.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "test",
        &tree,
        &parents,
    )
    .unwrap()
}

#[test]
fn compares_pinned_commits_in_order_and_resolves_moved_refs() {
    let directory = TempDir::new().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let first = commit(&repo, None, None);
    let second = commit(&repo, Some("hello"), Some(first));
    std::fs::write(directory.path().join("file.txt"), "uncommitted").unwrap();
    let index_before = std::fs::read(repo.path().join("index")).unwrap();
    repo.reference("refs/heads/selected", first, true, "test")
        .unwrap();
    let scope = Scope::new(vec![directory.path().to_path_buf()]).unwrap();
    let resolve = || {
        resolve_revision(
            &scope,
            ResolveRevision {
                repo: directory.path().to_path_buf(),
                revision: "refs/heads/selected".into(),
            },
        )
        .unwrap()
    };
    let pinned = resolve()["oid"].as_str().unwrap().to_owned();
    repo.reference("refs/heads/selected", second, true, "test")
        .unwrap();
    assert_eq!(resolve()["oid"], second.to_string());
    let compare = |from: String, to: String, common_ancestor| {
        compare_revisions(
            &scope,
            CompareRevisions {
                repo: directory.path().to_path_buf(),
                from,
                to,
                common_ancestor,
                offset: 0,
            },
        )
        .unwrap()
    };
    let forward = compare(pinned.clone(), second.to_string(), false);
    assert_eq!(forward["files"][0]["status"], "A");
    assert_eq!(forward["files"][0]["path"], "file.txt");
    let reverse = compare(second.to_string(), pinned.clone(), false);
    assert_eq!(reverse["files"][0]["status"], "D");
    assert!(
        compare(pinned.clone(), pinned, false)["files"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        compare(second.to_string(), first.to_string(), true)["base_oid"],
        first.to_string()
    );
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        index_before
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("file.txt")).unwrap(),
        "uncommitted"
    );
}

#[test]
fn rejects_unrelated_histories_and_repositories_outside_the_workspace() {
    let workspace = TempDir::new().unwrap();
    let repo = Repository::init(workspace.path()).unwrap();
    let first = commit(&repo, Some("hello"), None);
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let tree = repo.find_commit(first).unwrap().tree().unwrap();
    let unrelated = repo
        .commit(None, &signature, &signature, "unrelated", &tree, &[])
        .unwrap();
    let scope = Scope::new(vec![workspace.path().to_path_buf()]).unwrap();
    let error = compare_revisions(
        &scope,
        CompareRevisions {
            repo: workspace.path().to_path_buf(),
            from: first.to_string(),
            to: unrelated.to_string(),
            common_ancestor: true,
            offset: 0,
        },
    )
    .unwrap_err();
    assert!(error.message.contains("common ancestor"));
    let outside = TempDir::new().unwrap();
    Repository::init(outside.path()).unwrap();
    assert!(
        resolve_revision(
            &scope,
            ResolveRevision {
                repo: outside.path().to_path_buf(),
                revision: "HEAD".into(),
            }
        )
        .is_err()
    );
}

#[test]
fn large_comparisons_return_all_files_across_bounded_pages() {
    let directory = TempDir::new().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let first = commit(&repo, None, None);
    let mut index = repo.index().unwrap();
    for number in 0..257 {
        let name = format!("file-{number:03}.txt");
        std::fs::write(directory.path().join(&name), "content").unwrap();
        index.add_path(Path::new(&name)).unwrap();
    }
    let tree_id = index.write_tree().unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.find_commit(first).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let second = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "many",
            &tree,
            &[&parent],
        )
        .unwrap();
    let scope = Scope::new(vec![directory.path().to_path_buf()]).unwrap();
    let page = |offset| {
        compare_revisions(
            &scope,
            CompareRevisions {
                repo: directory.path().to_path_buf(),
                from: first.to_string(),
                to: second.to_string(),
                common_ancestor: false,
                offset,
            },
        )
        .unwrap()
    };
    let first_page = page(0);
    let last_page = page(256);
    assert_eq!(first_page["total"], 257);
    assert_eq!(first_page["next"], 256);
    assert_eq!(first_page["files"].as_array().unwrap().len(), 256);
    assert_eq!(last_page["files"].as_array().unwrap().len(), 1);
    assert!(last_page["next"].is_null());
}

#[test]
fn ref_picker_lists_branches_and_tags_beyond_the_first_page() {
    let directory = TempDir::new().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let commit = commit(&repo, None, None);
    for number in 0..257 {
        repo.reference(
            &format!("refs/heads/topic-{number:03}"),
            commit,
            true,
            "test",
        )
        .unwrap();
    }
    repo.reference("refs/tags/release", commit, true, "test")
        .unwrap();
    let scope = Scope::new(vec![directory.path().to_path_buf()]).unwrap();
    let page = |offset| {
        revision_refs(
            &scope,
            RevisionRefs {
                repo: directory.path().to_path_buf(),
                offset,
            },
        )
        .unwrap()
    };
    let first = page(0);
    let last = page(256);
    assert_eq!(first["revision"], last["revision"]);
    assert_eq!(first["next"], 256);
    let refs: Vec<_> = first["refs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(last["refs"].as_array().unwrap())
        .collect();
    assert!(refs.iter().any(|item| item["name"] == "refs/heads/topic-256" && item["oid"] == commit.to_string()));
    assert!(
        refs.iter()
            .any(|item| item["name"] == "refs/tags/release" && item["oid"] == commit.to_string())
    );
}
