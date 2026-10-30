use super::*;

#[test]
fn warned_submodule_is_visible_without_making_parent_dirty() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temporary, path, repo) = setup();
    let (_source_tmp, source, _source_repo) = setup();
    let output = crate::git::process::git_command(&path)
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "--"])
        .arg(crate::repo_id::boundary_path(&source).as_ref())
        .arg("child")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    commit_index(&repo, "add submodule");

    let child_path = path.join("child");
    let child = Repository::open(&child_path).unwrap();
    fs::write(child_path.join("new.txt"), "local commit\n").unwrap();
    let mut child_index = child.index().unwrap();
    child_index.add_path(Path::new("new.txt")).unwrap();
    child_index.write().unwrap();
    commit_index(&child, "unpushed child");
    let mut submodule = repo.find_submodule("child").unwrap();
    submodule.reload(true).unwrap();
    submodule.add_to_index(true).unwrap();
    commit_index(&repo, "record child");

    let options = SubmoduleConfig::default();
    let status = crate::git::status::query_status(&path, &options).unwrap();
    assert!(!status.is_dirty, "unexpected status: {status:?}");
    assert_eq!(status.file_count_lower_bound, 0);
    assert!(status.has_unpushed_submodules);
    let scope = Scope::new(vec![path.clone()]).unwrap();
    let mut store = Snapshots::default();
    let result = begin(&mut store, &scope, &path);
    let records = result["changes"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    let attention = item(records, "child", "attention");
    assert_eq!(attention["kind"], "submodule_attention");
    assert_eq!(attention["is_submodule"], true);
    assert_eq!(attention["submodule"]["warn"]["pointer_unreachable"], true);
    assert!(attention["submodule"]["state"].is_null());
    assert!(attention["original_path"].is_null());
    assert!(attention["conflict_stages"].is_null());
    let token = result["snapshot"].as_str().unwrap();
    let find = |snapshot: &str| FindRequest {
        repo: path.clone(),
        snapshot: snapshot.to_owned(),
        path: "child".to_owned(),
        group: "attention".to_owned(),
    };
    assert_eq!(
        store.find(&scope, find(token)).unwrap()["change"],
        *attention
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = _temporary.path().join("outside-child");
        fs::rename(&child_path, &outside).unwrap();
        symlink(&outside, &child_path).unwrap();
        assert_eq!(
            store.find(&scope, find(token)).unwrap()["change"],
            *attention
        );
        fs::remove_file(&child_path).unwrap();
        fs::rename(&outside, &child_path).unwrap();
    }

    let disabled = SubmoduleConfig {
        warn_unpushed: false,
        ..options.clone()
    };
    let disabled_page = store
        .begin(&scope, Begin { repo: path.clone() }, &disabled)
        .unwrap();
    assert!(disabled_page["changes"].as_array().unwrap().is_empty());
    assert_eq!(
        store.find(&scope, find(token)).unwrap_err().code,
        "snapshot_stale"
    );

    fs::write(child_path.join("mixed.txt"), "dirty\n").unwrap();
    let dirty = begin(&mut store, &scope, &path);
    let child_records: Vec<_> = dirty["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["path"] == "child")
        .collect();
    assert!(!child_records.is_empty());
    assert_ne!(dirty["revision"], result["revision"]);
    assert!(
        child_records
            .iter()
            .all(|record| record["submodule"]["state"] == "dirty")
    );
    assert!(
        child_records
            .iter()
            .all(|record| record["submodule"]["warn"]["pointer_unreachable"] == true)
    );
    assert!(
        child_records
            .iter()
            .all(|record| record["group"] != "attention")
    );

    let ignore_dirty = SubmoduleConfig {
        ignore_dirty: true,
        ..options.clone()
    };
    let ignored = store
        .begin(&scope, Begin { repo: path.clone() }, &ignore_dirty)
        .unwrap();
    let ignored_records = ignored["changes"].as_array().unwrap();
    assert_eq!(ignored_records.len(), 1);
    assert_eq!(
        item(ignored_records, "child", "attention")["kind"],
        "submodule_attention"
    );

    fs::write(child_path.join("mixed.txt"), "initial\n").unwrap();
    fs::write(child_path.join("second.txt"), "second local commit\n").unwrap();
    let mut index = child.index().unwrap();
    index.add_path(Path::new("second.txt")).unwrap();
    index.write().unwrap();
    commit_index(&child, "second unpushed child");
    submodule.reload(true).unwrap();
    submodule.add_to_index(true).unwrap();
    let staged = begin(&mut store, &scope, &path);
    let staged_records = staged["changes"].as_array().unwrap();
    let staged_child = item(staged_records, "child", "index");
    assert_eq!(staged_child["is_submodule"], true);
    let warn = &staged_child["submodule"]["warn"];
    assert_eq!(warn["pointer_unreachable"], true);
    assert!(
        !staged_records
            .iter()
            .any(|record| record["group"] == "attention")
    );
    commit_index(&repo, "record second child");
    let head = child.head().unwrap();
    let branch = child
        .find_branch(head.shorthand().unwrap(), git2::BranchType::Local)
        .unwrap();
    let upstream = branch.upstream().unwrap();
    child
        .reference(
            upstream.get().name().unwrap(),
            head.target().unwrap(),
            true,
            "published",
        )
        .unwrap();
    let published = begin(&mut store, &scope, &path);
    assert!(published["changes"].as_array().unwrap().is_empty());
    assert!(
        !crate::git::status::query_status(&path, &options)
            .unwrap()
            .has_unpushed_submodules
    );
}

#[cfg(unix)]
#[test]
fn replaced_submodules_keep_parent_status_and_change_navigation_available() {
    for nested in [false, true] {
        for removed_from_index in [false, true] {
            for dangling in [false, true] {
                let (_temporary, path, repo) = setup();
                let (_source_temp, source, source_repo) = setup();
                if nested {
                    let (_nested_temp, nested_source, _nested_repo) = setup();
                    let mut module = source_repo
                        .submodule(nested_source.to_str().unwrap(), Path::new("nested"), true)
                        .unwrap();
                    module.clone(None).unwrap();
                    module.add_finalize().unwrap();
                    drop(module);
                    commit_index(&source_repo, "Add nested child");
                }
                let mut module = repo
                    .submodule(source.to_str().unwrap(), Path::new("child"), true)
                    .unwrap();
                module.clone(None).unwrap();
                module.add_finalize().unwrap();
                drop(module);
                commit_index(&repo, "Add child");
                let replacement = if nested {
                    path.join("child/nested")
                } else {
                    path.join("child")
                };
                if removed_from_index {
                    let owner = if nested {
                        Repository::open(path.join("child")).unwrap()
                    } else {
                        Repository::open(&path).unwrap()
                    };
                    let mut index = owner.index().unwrap();
                    index
                        .remove_path(Path::new(if nested { "nested" } else { "child" }))
                        .unwrap();
                    index.write().unwrap();
                }
                if replacement.exists() {
                    fs::remove_dir_all(&replacement).unwrap();
                }
                let outside = TempDir::new().unwrap();
                let destination = outside.path().join(if dangling {
                    "missing"
                } else {
                    "corrupt-repository"
                });
                if !dangling {
                    let outside_repo = Repository::init(&destination).unwrap();
                    fs::write(outside_repo.path().join("index"), "invalid index").unwrap();
                }
                std::os::unix::fs::symlink(&destination, &replacement).unwrap();
                fs::write(path.join("mixed.txt"), "parent edit\n").unwrap();
                let scope = Scope::new(vec![path.clone()]).unwrap();
                let status = crate::bridge::status::query(
                    &path,
                    &scope,
                    &SubmoduleConfig::default(),
                    false,
                    true,
                    &[PathBuf::from("mixed.txt")],
                )
                .unwrap();
                assert_eq!(status["is_dirty"], true);
                assert_eq!(status["files"][0]["path"], "mixed.txt");
                assert_eq!(status["files"][0]["unstaged"], true);
                let mut store = Snapshots::default();
                let result = begin(&mut store, &scope, &path);
                let change = item(
                    result["changes"].as_array().unwrap(),
                    "mixed.txt",
                    "working",
                );
                assert_eq!(change["kind"], "working_modified");
                let found = store
                    .find(
                        &scope,
                        FindRequest {
                            repo: path.clone(),
                            snapshot: result["snapshot"].as_str().unwrap().into(),
                            path: "mixed.txt".into(),
                            group: "working".into(),
                        },
                    )
                    .unwrap();
                assert_eq!(found["change"], *change);
                assert!(
                    scope.validate_submodules(&path).is_err(),
                    "mutation validation must remain strict"
                );
            }
        }
    }
}
