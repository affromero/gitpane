use super::*;

#[test]
fn pointer_comparison_expires_when_submodule_diagnostics_change() {
    if !crate::git::git_test_available() {
        return;
    }
    let (_temp, path, repo, scope) = setup();
    let (_source_temp, source, _source_repo, _) = setup();
    let result = crate::git::process::git_command(&path)
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "--"])
        .arg(&source)
        .arg("child")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    commit(&repo);

    let child_path = path.join("child");
    let child = Repository::open(&child_path).unwrap();
    std::fs::write(child_path.join("seed"), "child change").unwrap();
    let mut index = child.index().unwrap();
    index.add_path(Path::new("seed")).unwrap();
    index.write().unwrap();
    commit(&child);
    let mut submodule = repo.find_submodule("child").unwrap();
    submodule.reload(true).unwrap();
    submodule.add_to_index(true).unwrap();

    let selected = request(&repo, &path, "child", "index");
    let before = selected.record["submodule"]["head"].clone();
    let snapshot = compare(
        &scope,
        CompareRequest {
            repo: selected.repo.clone(),
            record: selected.record.clone(),
            expected_repo_identity: selected.expected_repo_identity.clone(),
        },
        &SubmoduleConfig::default(),
    )
    .unwrap();
    let oid = child.head().unwrap().target().unwrap();
    child
        .branch("different-name", &child.find_commit(oid).unwrap(), false)
        .unwrap();
    child.set_head("refs/heads/different-name").unwrap();
    assert_eq!(child.head().unwrap().target(), Some(oid));
    let after = changes::current_record(&repo, &SubmoduleConfig::default(), "child", "index")
        .unwrap()
        .unwrap();
    assert_ne!(after["submodule"]["head"], before);
    assert_eq!(
        read(&scope, &selected, &snapshot, "index")
            .unwrap_err()
            .code,
        "stale_target"
    );
}
