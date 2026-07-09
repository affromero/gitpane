use super::test_support::*;
use super::*;
use std::{fs, path::Path};
use tempfile::TempDir;

/// Strip the git environment a hook injects (`GIT_DIR`, `GIT_INDEX_FILE`, …).
/// This suite runs under the pre-push hook, whose env would otherwise redirect
/// a spawned `git` at the outer repo instead of the test's temp repo.
fn clear_inherited_git_env(cmd: &mut std::process::Command) {
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        cmd.env_remove(var);
    }
}

#[test]
fn test_ahead_count_when_no_upstream_with_remote_ref() {
    // Local branch with no configured upstream but with a remote-tracking
    // ref at commit A. After two new local commits B, C, ahead == 2.
    let (tmp, repo) = init_temp_repo();
    let a = repo.head().unwrap().target().unwrap();
    repo.reference("refs/remotes/origin/main", a, true, "test setup")
        .unwrap();
    add_commit_on_head(&repo, "b.txt", "b");
    add_commit_on_head(&repo, "c.txt", "c");

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.ahead, 2);
    assert_eq!(status.behind, 0);
}

#[test]
fn test_no_remotes_keeps_zero_ahead() {
    // Repo with no remote-tracking refs at all. Two commits beyond the
    // initial one should report ahead == 0 because "unpushed" needs a
    // remote to mean anything.
    let (tmp, repo) = init_temp_repo();
    add_commit_on_head(&repo, "b.txt", "b");
    add_commit_on_head(&repo, "c.txt", "c");

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.ahead, 0);
    assert_eq!(status.behind, 0);
}

#[test]
fn test_stash_count_zero_when_no_stash() {
    let (tmp, _repo) = init_temp_repo();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.stashes.is_empty());
    assert_eq!(status.stash_count(), 0);
}

#[test]
fn test_stash_count_reflects_stash_save() {
    let (tmp, mut repo) = init_temp_repo();
    // Add a tracked file in an initial commit so stash has a baseline.
    add_commit_on_head(&repo, "tracked.txt", "v1");

    // Modify it and stash.
    fs::write(tmp.path().join("tracked.txt"), "v2").unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    repo.stash_save2(&sig, None, None).unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.stash_count(), 1);
    assert!(!status.stashes[0].oid.is_empty());

    // Stash again.
    fs::write(tmp.path().join("tracked.txt"), "v3").unwrap();
    repo.stash_save2(&sig, None, None).unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.stash_count(), 2);
    // Indices increase with insertion order so older stash is at higher index in libgit2.
    assert!(status.stashes.iter().any(|s| s.index == 0));
    assert!(status.stashes.iter().any(|s| s.index == 1));
}

#[test]
fn test_ahead_count_when_head_shares_only_root_with_unrelated_remote() {
    // HEAD chain: A -> B -> C. Remote tip: D (a sibling of B, off A).
    // The merge-base of HEAD and the remote tip is A. Walking C and
    // hiding D hides D and A; C and B remain. Expect ahead == 2.
    let (tmp, repo) = init_temp_repo();
    let a = repo.head().unwrap().target().unwrap();

    // Build an unrelated sibling commit D off A on a detached state.
    let a_commit = repo.find_commit(a).unwrap();
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let workdir = repo.workdir().unwrap().to_path_buf();
    fs::write(workdir.join("d.txt"), "d").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("d.txt")).unwrap();
    let d_tree_id = index.write_tree().unwrap();
    let d_tree = repo.find_tree(d_tree_id).unwrap();
    let d = repo
        .commit(None, &sig, &sig, "D", &d_tree, &[&a_commit])
        .unwrap();
    // Reset the index so the next HEAD commits don't carry d.txt.
    index.remove_path(Path::new("d.txt")).unwrap();
    index.write().unwrap();
    fs::remove_file(workdir.join("d.txt")).unwrap();

    // Publish D as a remote tip.
    repo.reference("refs/remotes/origin/other", d, true, "test setup")
        .unwrap();

    // Advance HEAD with B and C off A.
    add_commit_on_head(&repo, "b.txt", "b");
    add_commit_on_head(&repo, "c.txt", "c");

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert_eq!(status.ahead, 2);
    assert_eq!(status.behind, 0);
}

#[test]
fn fingerprint_changes_when_other_branch_moves() {
    // Reproduces the worktree case: a commit lands on a branch that is not
    // the checked-out HEAD. The root HEAD stays put, but the graph renders
    // that branch, so its fingerprint must change to drive a reload.
    let (_tmp, repo) = init_temp_repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("feature", &base, false).unwrap();

    let before = graph_refs_fingerprint(&repo);
    let head_before = repo.head().unwrap().target();

    // New commit on `feature` only — HEAD (master/main) is never updated,
    // exactly as a commit made inside a linked worktree would behave.
    let sig = git2::Signature::now("Test", "test@test.com").unwrap();
    let tree = repo.find_tree(base.tree_id()).unwrap();
    let moved = repo
        .commit(None, &sig, &sig, "on feature", &tree, &[&base])
        .unwrap();
    repo.reference("refs/heads/feature", moved, true, "move feature")
        .unwrap();

    let after = graph_refs_fingerprint(&repo);

    assert_ne!(
        before.local, after.local,
        "a moved local branch must change the local refs fingerprint"
    );
    assert_eq!(
        repo.head().unwrap().target(),
        head_before,
        "the checked-out HEAD must be untouched by the other-branch commit"
    );
}

#[test]
fn fingerprint_changes_when_branch_added_at_existing_commit() {
    // A new branch at an already-rendered commit changes the labels the
    // graph draws even though no new OID appears, so names must be hashed.
    let (_tmp, repo) = init_temp_repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap();

    let before = graph_refs_fingerprint(&repo);
    repo.branch("feature", &base, false).unwrap();
    let after = graph_refs_fingerprint(&repo);

    assert_ne!(before.local, after.local);
}

#[test]
fn fingerprint_buckets_remote_separately_from_local() {
    // A remote-only move must not perturb the local bucket, so the local
    // filter can ignore fetches.
    let (_tmp, repo) = init_temp_repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap();

    let before = graph_refs_fingerprint(&repo);
    repo.reference("refs/remotes/origin/feature", base.id(), true, "remote tip")
        .unwrap();
    let after = graph_refs_fingerprint(&repo);

    assert_eq!(
        before.local, after.local,
        "remote move must not touch local"
    );
    assert_ne!(before.remote, after.remote, "remote bucket must change");
}

#[test]
fn query_status_detects_commit_in_linked_worktree() {
    // End-to-end through the public entry point with a real linked worktree:
    // a commit made inside the worktree (on its own branch) must change the
    // root's `refs` fingerprint while leaving the root's checked-out HEAD
    // untouched — exactly the case the graph reload gate previously missed.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    let wt = tmp.path().join("wt-feature");
    fs::create_dir_all(&root).unwrap();

    let git = |args: &[&str], cwd: &Path| -> bool {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "t@t");
        // Clear the git env a hook injects (this suite runs under the pre-push
        // hook), so operations target `cwd`'s repo and not the outer one.
        clear_inherited_git_env(&mut cmd);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    };

    if !git(&["init", "-q"], &root) {
        eprintln!("skipping query_status_detects_commit_in_linked_worktree: git unavailable");
        return;
    }
    assert!(git(&["commit", "-q", "--allow-empty", "-m", "init"], &root));
    // Linked worktree checked out on its own branch (shares root refs/heads).
    assert!(git(
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feature"
        ],
        &root,
    ));

    let before = query_status(&root, &SubmoduleConfig::default()).unwrap();

    // Commit inside the worktree only — the root's HEAD never moves.
    assert!(git(
        &["commit", "-q", "--allow-empty", "-m", "in worktree"],
        &wt,
    ));

    let after = query_status(&root, &SubmoduleConfig::default()).unwrap();

    assert_eq!(
        before.head_oid, after.head_oid,
        "root checked-out HEAD must be unchanged by the worktree commit"
    );
    assert_ne!(
        before.refs.local, after.refs.local,
        "worktree commit moved a shared branch tip; the local refs \
         fingerprint must change so the graph reloads"
    );
}
