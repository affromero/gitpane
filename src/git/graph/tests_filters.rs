//! Follow-up coverage for the merged local↔upstream branch filter entry:
//! traversal modes (upstream-only refs, first-parent) and multi-remote
//! collapse resolution. See the base cases in `tests.rs`.

use super::tests::{create_commit, create_commit_no_ref};
use super::*;
use git2::Repository;
use tempfile::TempDir;

/// Init a repo where local `main` and `origin/main` diverge off `root`:
/// `local-new` is only on the local branch, `remote-new` only on the
/// remote-tracking ref, and `main` tracks `origin/main`.
fn diverged_tracked_repo(tmp: &TempDir) -> Repository {
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    {
        let root = repo.find_commit(root_oid).unwrap();
        repo.branch("main", &root, false).unwrap();

        let local_oid = create_commit_no_ref(&repo, "local-new", &[&root]);
        repo.reference("refs/heads/main", local_oid, true, "test setup")
            .unwrap();
        let remote_oid = create_commit_no_ref(&repo, "remote-new", &[&root]);
        repo.reference("refs/remotes/origin/main", remote_oid, true, "test setup")
            .unwrap();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
    }

    {
        let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
        main_branch.set_upstream(Some("origin/main")).unwrap();
    }
    repo
}

fn select_main(refs: GraphRefFilters, first_parent: bool) -> GraphOptions {
    GraphOptions {
        filters: GraphFilters {
            branches: Some(["main".to_string()].into_iter().collect()),
            authors: None,
            refs,
        },
        first_parent,
        ..GraphOptions::default()
    }
}

#[test]
fn merged_selection_with_local_refs_disabled_walks_only_the_upstream() {
    let tmp = TempDir::new().unwrap();
    diverged_tracked_repo(&tmp);

    let refs = GraphRefFilters {
        local: false,
        ..GraphRefFilters::default()
    };
    let rows = GraphBuilder::new()
        .build(tmp.path(), &select_main(refs, false))
        .unwrap();

    let messages: Vec<&str> = rows.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages.contains(&"remote-new"),
        "upstream tip must still be walked; got: {messages:?}"
    );
    assert!(
        !messages.contains(&"local-new"),
        "local tip must be dropped when refs.local is off; got: {messages:?}"
    );
}

#[test]
fn merged_selection_under_first_parent_keeps_both_tips_as_roots() {
    let tmp = TempDir::new().unwrap();
    let repo = diverged_tracked_repo(&tmp);

    // Merge a side commit into local `main` so first-parent simplification has
    // a second-parent chain to drop.
    let root = repo
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .parents()
        .next()
        .unwrap();
    let side_oid = create_commit_no_ref(&repo, "side-branch", &[&root]);
    let side = repo.find_commit(side_oid).unwrap();
    let local_tip = repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let merge_oid = create_commit_no_ref(&repo, "merge-side", &[&local_tip, &side]);
    repo.reference("refs/heads/main", merge_oid, true, "test setup")
        .unwrap();

    let rows = GraphBuilder::new()
        .build(tmp.path(), &select_main(GraphRefFilters::default(), true))
        .unwrap();

    let messages: Vec<&str> = rows.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages.contains(&"merge-side") && messages.contains(&"local-new"),
        "local first-parent chain must be walked; got: {messages:?}"
    );
    assert!(
        messages.contains(&"remote-new"),
        "upstream tip must stay a root under first-parent; got: {messages:?}"
    );
    assert!(
        !messages.contains(&"side-branch"),
        "second-parent chain must be simplified away; got: {messages:?}"
    );
}

#[test]
fn same_leaf_name_on_two_remotes_collapses_each_into_its_own_local() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();

    let root_oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(root_oid).unwrap();
    repo.branch("main", &root, false).unwrap();
    repo.branch("mirror", &root, false).unwrap();

    // Two remotes both carry a `main` leaf; `main` tracks origin's, `mirror`
    // tracks backup's. Each collapse must resolve by exact upstream name.
    repo.reference("refs/remotes/origin/main", root_oid, true, "test setup")
        .unwrap();
    repo.reference("refs/remotes/backup/main", root_oid, true, "test setup")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();
    repo.remote("backup", "https://example.com/backup.git")
        .unwrap();

    let mut main_branch = repo.find_branch("main", git2::BranchType::Local).unwrap();
    main_branch.set_upstream(Some("origin/main")).unwrap();
    let mut mirror_branch = repo.find_branch("mirror", git2::BranchType::Local).unwrap();
    mirror_branch.set_upstream(Some("backup/main")).unwrap();

    let names = GraphBuilder::branch_names(tmp.path(), &crate::config::BranchFilter::All).unwrap();
    assert!(names.iter().any(|n| n == "main"), "got: {names:?}");
    assert!(names.iter().any(|n| n == "mirror"), "got: {names:?}");
    assert!(
        !names
            .iter()
            .any(|n| n == "origin/main" || n == "backup/main"),
        "each tracked upstream must collapse into its own local; got: {names:?}"
    );
}

fn revision(path: &std::path::Path, filter: BranchFilter) -> String {
    GraphBuilder::revision_scoped(path, &filter, &|_| true).unwrap()
}

#[test]
fn graph_revision_follows_refs_catalog_head_and_stashes() {
    let tmp = TempDir::new().unwrap();
    let mut repo = Repository::init(tmp.path()).unwrap();
    let unborn = revision(tmp.path(), BranchFilter::All);
    let oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(oid).unwrap();
    let initial = revision(tmp.path(), BranchFilter::All);
    assert_ne!(unborn, initial);
    assert_eq!(initial, revision(tmp.path(), BranchFilter::All));

    repo.branch("feature", &root, false).unwrap();
    let branch = revision(tmp.path(), BranchFilter::All);
    assert_ne!(initial, branch);
    let second = create_commit_no_ref(&repo, "feature commit", &[&root]);
    repo.reference("refs/heads/feature", second, true, "test")
        .unwrap();
    let before_tag = revision(tmp.path(), BranchFilter::All);
    assert_ne!(branch, before_tag);
    repo.reference("refs/tags/v1", oid, false, "test").unwrap();
    let tag = revision(tmp.path(), BranchFilter::All);
    assert_ne!(before_tag, tag);

    repo.reference("refs/remotes/origin/feature", oid, false, "test")
        .unwrap();
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();
    let separate = revision(tmp.path(), BranchFilter::All);
    let mut feature = repo
        .find_branch("feature", git2::BranchType::Local)
        .unwrap();
    feature.set_upstream(Some("origin/feature")).unwrap();
    let tracking = revision(tmp.path(), BranchFilter::All);
    assert_ne!(separate, tracking);
    let local_only = revision(tmp.path(), BranchFilter::Local);
    repo.reference("refs/remotes/origin/other", oid, false, "test")
        .unwrap();
    assert_eq!(local_only, revision(tmp.path(), BranchFilter::Local));
    assert_ne!(tracking, revision(tmp.path(), BranchFilter::All));

    let attached = revision(tmp.path(), BranchFilter::All);
    repo.set_head_detached(oid).unwrap();
    let detached = revision(tmp.path(), BranchFilter::All);
    assert_ne!(detached, attached);
    repo.set_head("refs/heads/feature").unwrap();
    assert_ne!(detached, revision(tmp.path(), BranchFilter::All));

    std::fs::write(tmp.path().join("stash.txt"), "stashed\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("stash.txt")).unwrap();
    index.write().unwrap();
    drop(index);
    let before_stash = revision(tmp.path(), BranchFilter::All);
    let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
    drop(feature);
    drop(root);
    repo.stash_save2(&signature, Some("test stash"), None)
        .unwrap();
    assert_ne!(before_stash, revision(tmp.path(), BranchFilter::All));
}

#[test]
fn graph_revision_accepts_noncommit_tags_and_rejects_missing_objects() {
    let tmp = TempDir::new().unwrap();
    let repo = Repository::init(tmp.path()).unwrap();
    create_commit(&repo, "root", &[]);
    let blob_id = repo.blob(b"tagged content").unwrap();
    repo.reference("refs/tags/blob", blob_id, false, "test")
        .unwrap();
    let blob = repo.find_object(blob_id, None).unwrap();
    let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
    repo.tag("annotated-blob", &blob, &signature, "test", false)
        .unwrap();
    revision(tmp.path(), BranchFilter::All);

    let bad_ref = tmp.path().join(".git/refs/tags/missing");
    std::fs::write(bad_ref, "1111111111111111111111111111111111111111\n").unwrap();
    let error =
        GraphBuilder::revision_scoped(tmp.path(), &BranchFilter::All, &|_| true).unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn graph_revision_respects_worktree_scope() {
    if !crate::git::git_test_available() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let repo_path = tmp.path().join("repo");
    std::fs::create_dir(&repo_path).unwrap();
    let repo = Repository::init(&repo_path).unwrap();
    let oid = create_commit(&repo, "root", &[]);
    let root = repo.find_commit(oid).unwrap();
    repo.branch("feature", &root, false).unwrap();
    let included =
        || GraphBuilder::revision_scoped(&repo_path, &BranchFilter::All, &|_| true).unwrap();
    let excluded =
        || GraphBuilder::revision_scoped(&repo_path, &BranchFilter::All, &|_| false).unwrap();
    let before_included = included();
    let before_excluded = excluded();
    let output = crate::git::process::git_command(&repo_path)
        .args(["worktree", "add"])
        .arg(tmp.path().join("linked"))
        .arg("feature")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(before_included, included());
    assert_eq!(before_excluded, excluded());
}
