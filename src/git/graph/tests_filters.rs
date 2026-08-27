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
