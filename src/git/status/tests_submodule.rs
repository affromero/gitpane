use super::test_support::*;
use super::*;
use git2::Repository;
use std::{fs, path::Path};

#[test]
fn test_dirty_submodule_with_real_git_submodule() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // Verify: clean state should show has_submodules but no dirty submodules
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_submodules);
    assert!(!status.has_dirty_submodules);
    assert!(status.submodules.is_empty());

    // Now make the submodule dirty by modifying a file inside it
    let sub_workdir = tmp.path().join("my-sub");
    fs::write(sub_workdir.join("lib.rs"), "fn hello() { /* changed */ }").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_submodules);
    assert!(status.has_dirty_submodules);
    assert!(!status.submodules.is_empty());

    let sub_info = &status.submodules[0];
    assert_eq!(sub_info.path, Path::new("my-sub"));
    assert_eq!(sub_info.state, Some(SubmoduleState::Dirty));

    // Verify the file entry is annotated
    let file_entry = status.files.iter().find(|f| f.path == Path::new("my-sub"));
    assert!(file_entry.is_some());
    let file_entry = file_entry.unwrap();
    assert!(file_entry.is_submodule);
    assert_eq!(file_entry.submodule_state, Some(SubmoduleState::Dirty));
}

#[test]
fn test_ignore_dirty_subs_hides_submodule_state() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // Make the submodule dirty
    fs::write(tmp.path().join("my-sub/lib.rs"), "fn changed() {}").unwrap();

    // With ignore_dirty_subs = true, submodule state should be hidden
    let status = query_status(
        tmp.path(),
        &SubmoduleConfig {
            ignore_dirty: true,
            warn_unpushed: false,
        },
    )
    .unwrap();
    assert!(status.has_submodules); // .gitmodules still exists
    assert!(!status.has_dirty_submodules);
    assert!(status.submodules.is_empty());
    // No submodule-annotated entries
    assert!(!status.files.iter().any(|f| f.is_submodule));
}

#[test]
fn test_submodule_modified_pointer() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    add_unpushed_commit_in_sub(tmp.path(), "my-sub");

    // Now the submodule pointer has changed (HEAD in submodule != recorded in parent)
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_submodules);
    assert!(status.has_dirty_submodules);
    assert!(!status.submodules.is_empty());

    let sub_info = &status.submodules[0];
    assert_eq!(sub_info.path, Path::new("my-sub"));
    // Could be Modified or Dirty depending on exact git state
    assert!(
        sub_info.state == Some(SubmoduleState::Modified)
            || sub_info.state == Some(SubmoduleState::Dirty),
        "expected Modified or Dirty, got {:?}",
        sub_info.state
    );

    // Verify OIDs are populated
    assert!(sub_info.head_oid.is_some());
    assert!(sub_info.workdir_oid.is_some());
    assert_ne!(sub_info.head_oid, sub_info.workdir_oid);
}

#[test]
fn test_clean_submodule_not_reported() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // Without any modifications, the submodule should be clean
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_submodules);
    assert!(!status.has_dirty_submodules);
    assert!(status.submodules.is_empty());
    assert!(!status.files.iter().any(|f| f.is_submodule));
}

#[test]
fn test_dirty_submodule_makes_repo_dirty() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // Start clean
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(!status.is_dirty);

    // Make submodule dirty
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

    // Now repo should be dirty
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.is_dirty);
}

#[test]
fn test_submodule_warn_default_is_clean() {
    let warn = SubmoduleWarn::default();
    assert!(warn.is_clean());
    assert_eq!(warn.unpushed_commits, 0);
    assert!(!warn.pointer_unreachable);
}

#[test]
fn test_submodule_warn_is_clean_predicate() {
    assert!(SubmoduleWarn::default().is_clean());
    assert!(
        !SubmoduleWarn {
            unpushed_commits: 1,
            pointer_unreachable: false,
            needs_merge_to_default: false,
        }
        .is_clean()
    );
    assert!(
        !SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: true,
            needs_merge_to_default: false,
        }
        .is_clean()
    );
    assert!(
        !SubmoduleWarn {
            unpushed_commits: 0,
            pointer_unreachable: false,
            needs_merge_to_default: true,
        }
        .is_clean()
    );
}

#[test]
fn test_clean_submodule_warn_fields_clean() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    // Freshly cloned via `git submodule add` — recorded oid is on origin.
    assert!(!status.has_unpushed_submodules);
}

#[test]
fn test_dirty_submodule_warn_fields_stay_clean() {
    // A workdir-dirty submodule should not trigger warn fields — only
    // pointer changes / unpushed commits matter for "you owe a push".
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_dirty_submodules);
    assert!(!status.has_unpushed_submodules);
    let sub_info = &status.submodules[0];
    assert!(sub_info.warn.is_clean());

    let file_entry = status
        .files
        .iter()
        .find(|f| f.path == Path::new("my-sub"))
        .unwrap();
    assert!(file_entry.submodule_warn.is_clean());
}

#[test]
fn test_warn_unpushed_false_zeros_warn_fields_even_when_dirty() {
    // With warn_unpushed=false, dirty subs still surface but warn fields stay zero.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

    let cfg = SubmoduleConfig {
        ignore_dirty: false,
        warn_unpushed: false,
    };
    let status = query_status(tmp.path(), &cfg).unwrap();
    assert!(status.has_dirty_submodules);
    assert!(!status.has_unpushed_submodules);
    assert!(status.submodules[0].warn.is_clean());
}

#[test]
fn test_ignore_dirty_with_warn_unpushed_iterates_subs() {
    // When ignore_dirty=true but warn_unpushed=true, the loop must still
    // iterate submodules to compute warn fields. Dirty state itself is hidden.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

    let cfg = SubmoduleConfig {
        ignore_dirty: true,
        warn_unpushed: true,
    };
    let status = query_status(tmp.path(), &cfg).unwrap();
    // Dirty signal hidden
    assert!(!status.has_dirty_submodules);
    // Warn signal also clean for this freshly-added submodule
    assert!(!status.has_unpushed_submodules);
    // No submodule entries (nothing to warn about; dirty hidden)
    assert!(status.submodules.is_empty());
}

#[test]
fn test_both_flags_off_skips_submodule_loop_entirely() {
    // When both flags are off, the loop body is skipped — fast path.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    fs::write(tmp.path().join("my-sub/lib.rs"), "dirty").unwrap();

    let cfg = SubmoduleConfig {
        ignore_dirty: true,
        warn_unpushed: false,
    };
    let status = query_status(tmp.path(), &cfg).unwrap();
    assert!(status.has_submodules); // .gitmodules still exists
    assert!(!status.has_dirty_submodules);
    assert!(!status.has_unpushed_submodules);
    assert!(status.submodules.is_empty());
}

#[test]
fn test_uninitialized_submodule_does_not_panic_on_warn_check() {
    // De-init the submodule so .git is absent — `sub.open()` should fail
    // gracefully and `compute_submodule_warn` returns default.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    fs::remove_dir_all(tmp.path().join("my-sub")).unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_submodules);
    // No panic. The submodule may or may not appear in `submodules` (the
    // dirty-state check still classifies it as Uninitialized) — the key
    // invariant is that warn fields stay clean.
    for sub in &status.submodules {
        assert!(sub.warn.is_clean());
    }
    assert!(!status.has_unpushed_submodules);
}

#[test]
fn test_parent_pinning_unpushed_oid_marks_pointer_unreachable() {
    // Reproduce the footgun: commit in submodule (not on remote), then
    // `git add my-sub` in parent — staging an oid that no remote can resolve.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    let unpushed = add_unpushed_commit_in_sub(tmp.path(), "my-sub");

    // Stage the new submodule pointer in the parent's index.
    stage_submodule_pointer(tmp.path(), "my-sub");

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_unpushed_submodules);

    let sub_info = status
        .submodules
        .iter()
        .find(|s| s.path == Path::new("my-sub"))
        .expect("submodule entry should exist with warn signal");
    assert!(
        sub_info.warn.pointer_unreachable,
        "expected pointer_unreachable=true for staged oid {}, got {:?}",
        unpushed, sub_info.warn
    );

    // The file row should also surface the warn fields.
    let file_entry = status
        .files
        .iter()
        .find(|f| f.path == Path::new("my-sub"))
        .expect("file entry for my-sub");
    assert!(file_entry.is_submodule);
    assert!(file_entry.submodule_warn.pointer_unreachable);
}

#[test]
fn test_warn_unpushed_false_zeros_unreachable_pointer() {
    // Same setup as the previous test, but with warn_unpushed=false the
    // warn fields must stay zero even though the pointer is unreachable.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();
    add_unpushed_commit_in_sub(tmp.path(), "my-sub");
    stage_submodule_pointer(tmp.path(), "my-sub");

    let cfg = SubmoduleConfig {
        ignore_dirty: false,
        warn_unpushed: false,
    };
    let status = query_status(tmp.path(), &cfg).unwrap();
    assert!(!status.has_unpushed_submodules);
    for sub in &status.submodules {
        assert!(
            sub.warn.is_clean(),
            "warn fields must be zero when warn_unpushed=false, got {:?}",
            sub.warn
        );
    }
}

#[test]
fn test_unpushed_commits_count_when_branch_ahead_of_upstream() {
    // The submodule's local branch advances past its upstream remote ref;
    // the parent's recorded oid is unchanged (still reachable on origin),
    // so unpushed_commits>0 with pointer_unreachable=false.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // Find the cloned submodule's default remote branch name. After
    // `git submodule add`, the inner repo has a `refs/remotes/origin/<branch>`
    // ref. Use it to set up a local tracking branch.
    let sub_dir = tmp.path().join("my-sub");
    let inner = Repository::open(&sub_dir).unwrap();
    let mut remote_branch: Option<String> = None;
    if let Ok(branches) = inner.branches(Some(git2::BranchType::Remote)) {
        for (b, _) in branches.flatten() {
            if let Ok(Some(name)) = b.name()
                && let Some(stripped) = name.strip_prefix("origin/")
                && stripped != "HEAD"
            {
                remote_branch = Some(stripped.to_string());
                break;
            }
        }
    }
    let branch = remote_branch.expect("submodule should have an origin/<branch> ref");

    // Create a local tracking branch pointing at HEAD and set its upstream.
    let remote_ref = inner
        .find_reference(&format!("refs/remotes/origin/{}", branch))
        .unwrap();
    let remote_oid = remote_ref.target().unwrap();
    let remote_commit = inner.find_commit(remote_oid).unwrap();
    let mut local_branch = inner
        .find_branch(&branch, git2::BranchType::Local)
        .or_else(|_| inner.branch(&branch, &remote_commit, false))
        .unwrap();
    local_branch
        .set_upstream(Some(&format!("origin/{}", branch)))
        .unwrap();
    inner.set_head(&format!("refs/heads/{}", branch)).unwrap();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    inner.checkout_head(Some(&mut checkout)).unwrap();

    // Add an unpushed commit (local branch advances past origin/<branch>).
    add_unpushed_commit_in_sub(tmp.path(), "my-sub");

    // The parent has not staged a new pointer, so its recorded oid is
    // still the prior commit — present on origin.
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(status.has_unpushed_submodules);

    let sub_info = status
        .submodules
        .iter()
        .find(|s| s.path == Path::new("my-sub"))
        .expect("submodule entry expected");
    assert_eq!(
        sub_info.warn.unpushed_commits, 1,
        "expected 1 unpushed commit, got {:?}",
        sub_info.warn
    );
    assert!(
        !sub_info.warn.pointer_unreachable,
        "parent's pointer is unchanged and on origin — should be reachable"
    );
}

#[test]
fn test_detached_head_at_remote_oid_no_warn() {
    // A submodule with detached HEAD at an oid present on a remote ref
    // should produce no warn signal.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    // After `git submodule add`, HEAD is typically already detached at the
    // initial cloned commit, which is on `refs/remotes/origin/<branch>`.
    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    assert!(!status.has_unpushed_submodules);
    for sub in &status.submodules {
        assert!(sub.warn.is_clean());
    }
}

#[test]
fn test_needs_merge_to_default_when_pinned_on_side_branch() {
    // The parent pins a submodule commit that lives on a remote *side*
    // branch (origin/feature) but is not reachable from the default branch
    // (origin/main). The commit is fetchable, so it is not "unreachable",
    // yet it still needs merging to main → `needs_merge_to_default`.
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    let sub_dir = tmp.path().join("my-sub");
    let inner = Repository::open(&sub_dir).unwrap();

    // A: the initial cloned commit. Anchor the default branch here.
    let a = inner.head().unwrap().peel_to_commit().unwrap().id();
    inner
        .reference("refs/remotes/origin/main", a, true, "test setup")
        .unwrap();

    // B: a new commit (child of A) now checked out in the submodule,
    // published only on origin/feature.
    let b = add_commit_on_head(&inner, "feature.rs", "fn f() {}");
    inner
        .reference("refs/remotes/origin/feature", b, true, "test setup")
        .unwrap();

    // Stage the parent's pointer at B (the submodule's current HEAD).
    stage_submodule_pointer(tmp.path(), "my-sub");

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    let sub = status
        .submodules
        .iter()
        .find(|s| s.path == Path::new("my-sub"))
        .expect("submodule entry expected");

    assert!(
        sub.warn.needs_merge_to_default,
        "pinned commit is on origin/feature, not origin/main: {:?}",
        sub.warn
    );
    assert!(
        !sub.warn.pointer_unreachable,
        "commit is on a remote, so it is reachable: {:?}",
        sub.warn
    );
    assert!(!sub.warn.is_clean());
    assert!(status.has_unpushed_submodules);
}

/// A pinned commit that IS on the default branch must not warn.
#[test]
fn test_no_merge_warning_when_pinned_on_default_branch() {
    let (tmp, _sub_source, _sub_repo) = init_repo_with_submodule();

    let sub_dir = tmp.path().join("my-sub");
    let inner = Repository::open(&sub_dir).unwrap();
    let a = inner.head().unwrap().peel_to_commit().unwrap().id();
    // Default branch points at the very commit the parent pins.
    inner
        .reference("refs/remotes/origin/main", a, true, "test setup")
        .unwrap();

    let status = query_status(tmp.path(), &SubmoduleConfig::default()).unwrap();
    for sub in &status.submodules {
        assert!(
            !sub.warn.needs_merge_to_default,
            "pinned commit is on origin/main: {:?}",
            sub.warn
        );
    }
}
