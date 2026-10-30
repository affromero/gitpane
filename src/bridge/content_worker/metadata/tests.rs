use std::fs;
use std::path::{Path, PathBuf};

use git2::{IndexEntry, Repository, Signature};

use super::{canonical_results, config, metadata, mode_results, supports_object_mode};
use crate::bridge::content_worker::{Item, Operation, Request};

fn item(reference: &str, path: &str) -> Item {
    Item {
        reference: reference.to_owned(),
        path: path.to_owned(),
    }
}

fn request(repo: &Path) -> Request {
    Request {
        version: 1,
        workspace_roots: Vec::new(),
        workspace_root_identities: Vec::new(),
        workspace_root_sources: Vec::new(),
        repo: repo.to_owned(),
        expected_repo_identity: "0".repeat(64),
        git_executable: PathBuf::from("git"),
        operation: Operation::Metadata { items: Vec::new() },
    }
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = crate::git::process::git_command(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn canonical_metadata_observes_replacements_stages_and_configuration() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let index_path = if cfg!(windows) { "0foo" } else { "0:foo" };
    fs::write(root.join(index_path), b"index colon").unwrap();
    fs::write(root.join("file[1]"), b"old").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(index_path)).unwrap();
    index.add_path(Path::new("file[1]")).unwrap();
    index.write().unwrap();
    let original = index.get_path(Path::new("file[1]"), 0).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    drop(tree);

    let replacement = repo.blob(b"replacement bytes").unwrap();
    run_git(
        &root,
        &[
            "replace",
            &original.id.to_string(),
            &replacement.to_string(),
        ],
    );
    let items = [
        item("HEAD", "file[1]"),
        item(":0", index_path),
        item(":2", "file[1]"),
        item("HEAD", "missing"),
    ];
    let first = metadata(&request(&root), &items).unwrap();
    if supports_object_mode(&request(&root)).unwrap() {
        assert_eq!(
            mode_results(&request(&root), &items).unwrap(),
            canonical_results(&request(&root), &items).unwrap()
        );
    }
    assert_eq!(
        first["results"][0]["metadata"]["oid"],
        original.id.to_string()
    );
    assert_eq!(first["results"][0]["metadata"]["size"], 17);
    assert_eq!(
        first["results"][0]["metadata"]["head_oid"],
        commit.to_string()
    );
    assert_eq!(first["results"][1]["metadata"]["size"], 11);
    assert_eq!(first["results"][2]["error"]["code"], "content_not_found");
    assert_eq!(first["results"][3]["error"]["code"], "content_not_found");
    let absent = "0".repeat(40);
    let blob = original.id.to_string();
    let mixed = metadata(
        &request(&root),
        &[
            item(&absent, "file[1]"),
            item(&blob, "file[1]"),
            item("HEAD", "file[1]"),
        ],
    )
    .unwrap();
    assert_eq!(mixed["results"][0]["error"]["code"], "content_not_found");
    assert_eq!(mixed["results"][1]["error"]["code"], "unsupported_content");
    assert_eq!(mixed["results"][2]["metadata"]["size"], 17);

    index.remove_path(Path::new("file[1]")).unwrap();
    let conflict_oid = repo.blob(b"ours contents").unwrap();
    let mut conflict = IndexEntry {
        id: conflict_oid,
        ..original
    };
    conflict.flags = (conflict.flags & !(3 << 12)) | (2 << 12);
    index.add(&conflict).unwrap();
    index.write().unwrap();
    let staged = metadata(
        &request(&root),
        &[item(":2", "file[1]"), item(":0", "file[1]")],
    )
    .unwrap();
    assert_eq!(staged["results"][0]["metadata"]["size"], 13);
    assert_eq!(staged["results"][1]["error"]["code"], "content_not_found");

    let config_file = root.join("included.conf");
    fs::write(&config_file, "[diff \"custom\"]\ntextconv = first\n").unwrap();
    repo.config()
        .unwrap()
        .set_str("include.path", config_file.to_str().unwrap())
        .unwrap();
    let configured = metadata(&request(&root), &items).unwrap();
    assert_eq!(configured["volatile"], true);
    assert_ne!(first["config"], configured["config"]);
    fs::write(&config_file, "[diff \"custom\"]\ntextconv = second\n").unwrap();
    let changed = metadata(&request(&root), &items).unwrap();
    assert_ne!(configured["config"], changed["config"]);
    let next = repo.blob(b"replacement again").unwrap();
    run_git(
        &root,
        &["replace", "-f", &original.id.to_string(), &next.to_string()],
    );
    let replaced = metadata(&request(&root), &items).unwrap();
    assert_ne!(changed["replacements"], replaced["replacements"]);
}

#[test]
fn unborn_head_and_missing_index_paths_are_reported_individually() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    Repository::init(&root).unwrap();
    let result = metadata(
        &request(&root),
        &[item("HEAD", "missing"), item(":0", "missing")],
    )
    .unwrap();
    for entry in result["results"].as_array().unwrap() {
        assert_eq!(entry["error"]["code"], "content_not_found");
    }
}

#[test]
fn configuration_fingerprint_tracks_canonical_git_configuration() {
    if !crate::git::git_test_available() {
        return;
    }
    const MODE: &str = "GITPANE_TEST_CONFIG_MODE";
    const REPO: &str = "GITPANE_TEST_CONFIG_REPO";
    if let Some(mode) = std::env::var_os(MODE) {
        let root = PathBuf::from(std::env::var_os(REPO).unwrap());
        let req = request(&root);
        let included = root.join("included.conf");
        let global = root.join("global.conf");
        let (baseline, volatile) = config(&req).unwrap();
        if mode == "environment" {
            assert!(volatile, "environment-provided converter was not detected");
            return;
        }
        assert!(!volatile, "empty configuration should not be volatile");

        fs::write(
            &included,
            "[diff \"odd\\nname\"]\ntextconv = \"one\\ntwo\"\n",
        )
        .unwrap();
        let (newline, volatile) = config(&req).unwrap();
        assert!(volatile);
        assert_ne!(baseline, newline);
        fs::write(
            &included,
            "[diff \"odd\\nname\"]\ntextconv = \"one\\nchanged\"\n",
        )
        .unwrap();
        let (changed, _) = config(&req).unwrap();
        assert_ne!(newline, changed);

        fs::write(&included, "[diff \"empty\"]\ntextconv =\n").unwrap();
        let (empty, volatile) = config(&req).unwrap();
        assert!(
            volatile,
            "an empty converter value remains conservatively volatile"
        );
        fs::write(
            &included,
            "[diff \"empty\"]\ntextconv =\ntextconv = second\n",
        )
        .unwrap();
        let (duplicate, _) = config(&req).unwrap();
        assert_ne!(empty, duplicate);
        fs::write(
            &included,
            "[diff \"empty\"]\ntextconv =\ntextconv = changed\n",
        )
        .unwrap();
        let (changed_duplicate, _) = config(&req).unwrap();
        assert_ne!(duplicate, changed_duplicate);

        fs::write(&global, "[diff \"global\"]\ntextconv = converter\n").unwrap();
        let (with_global, _) = config(&req).unwrap();
        assert_ne!(changed_duplicate, with_global);
        fs::write(&included, "").unwrap();
        fs::write(&global, "[other]\ntextconv = unrelated\n").unwrap();
        assert!(
            config(&req).unwrap().1,
            "unrelated matching keys must be conservative"
        );
        fs::write(&global, "[invalid\n").unwrap();
        assert_eq!(config(&req).unwrap_err().code, "repository_error");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let included = root.join("included.conf");
    let global = root.join("global.conf");
    fs::write(&included, "").unwrap();
    fs::write(&global, "").unwrap();
    repo.config()
        .unwrap()
        .set_str(
            "include.path",
            crate::repo_id::boundary_path(&included).to_str().unwrap(),
        )
        .unwrap();
    for mode in ["normal", "environment"] {
        fs::write(&included, "").unwrap();
        fs::write(&global, "").unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "bridge::content_worker::metadata::tests::configuration_fingerprint_tracks_canonical_git_configuration", "--nocapture"])
            .env(MODE, mode).env(REPO, &root).env("GIT_CONFIG_GLOBAL", crate::repo_id::boundary_path(&global).as_ref())
            .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG", root.join("ignored.conf"))
            .env_remove("GIT_CONFIG_COUNT").env_remove("GIT_CONFIG_PARAMETERS");
        if mode == "environment" {
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "diff.env.textconv")
                .env("GIT_CONFIG_VALUE_0", "environment-converter");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn custom_replacement_namespace_updates_sizes_and_fingerprint() {
    if !crate::git::git_test_available() {
        return;
    }
    const CHILD: &str = "GITPANE_TEST_CUSTOM_REPLACEMENTS";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bridge::content_worker::metadata::tests::custom_replacement_namespace_updates_sizes_and_fingerprint", "--nocapture"])
            .env(CHILD, "1").env("GIT_REPLACE_REF_BASE", "refs/gitpane-test/")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    fs::write(root.join("file"), b"old").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.write().unwrap();
    let original = index.get_path(Path::new("file"), 0).unwrap().id;
    let replacement = repo.blob(b"custom replacement").unwrap();
    let before = metadata(&request(&root), &[item(":0", "file")]).unwrap();
    run_git(
        &root,
        &[
            "update-ref",
            &format!("refs/gitpane-test/{original}"),
            &replacement.to_string(),
        ],
    );
    let after = metadata(&request(&root), &[item(":0", "file")]).unwrap();
    assert_eq!(after["results"][0]["metadata"]["size"], 18);
    assert_eq!(after["volatile"], true);
    assert_ne!(before["replacements"], after["replacements"]);
}

#[cfg(unix)]
#[test]
fn newline_in_missing_path_does_not_corrupt_batch_results() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    fs::write(root.join("line\nbreak"), b"text").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("line\nbreak")).unwrap();
    index.write().unwrap();
    let result = metadata(
        &request(&root),
        &[item(":0", "missing\nname"), item(":0", "line\nbreak")],
    )
    .unwrap();
    assert_eq!(result["results"][0]["error"]["code"], "content_not_found");
    assert_eq!(result["results"][1]["metadata"]["size"], 4);
}

#[test]
fn missing_index_blob_is_not_reported_as_an_absent_path() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    if !supports_object_mode(&request(&root)).unwrap() {
        return;
    }
    fs::write(root.join("file"), b"unpacked object").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.write().unwrap();
    let object = index.get_path(Path::new("file"), 0).unwrap().id.to_string();
    fs::remove_file(
        root.join(".git/objects")
            .join(&object[..2])
            .join(&object[2..]),
    )
    .unwrap();
    assert_eq!(
        mode_results(&request(&root), &[item(":0", "file")])
            .unwrap_err()
            .code,
        "stale_target"
    );
    assert_eq!(
        mode_results(&request(&root), &[item(":0", "missing")]).unwrap()[0]["error"]["code"],
        "content_not_found"
    );
}

#[cfg(unix)]
#[test]
fn old_git_capability_uses_canonical_path_but_other_errors_surface() {
    use std::os::unix::fs::PermissionsExt;
    if !crate::git::git_test_available() {
        return;
    }
    const CHILD: &str = "GITPANE_TEST_LOCALIZED_CAPABILITY";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bridge::content_worker::metadata::tests::old_git_capability_uses_canonical_path_but_other_errors_surface", "--nocapture"])
            .env(CHILD, "1").env("LC_ALL", "fr_FR.UTF-8").env("LANGUAGE", "fr")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    fs::write(root.join("file"), b"contents").unwrap();
    let unusual = "line\n:0[?]*";
    fs::write(root.join(unusual), b"newline index").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.add_path(Path::new(unusual)).unwrap();
    let template = index.get_path(Path::new("file"), 0).unwrap();
    let gitlink = IndexEntry {
        id: git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
        mode: 0o160000,
        flags: 7,
        path: b"gitlink".to_vec(),
        ..template
    };
    index.add(&gitlink).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let commit = repo
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    let original = index.get_path(Path::new(unusual), 0).unwrap().id;
    let replacement = repo.blob(b"replacement newline contents").unwrap();
    run_git(
        &root,
        &["replace", &original.to_string(), &replacement.to_string()],
    );
    let executable = root.join("git-wrapper");
    fs::write(&executable, "#!/bin/sh\ncase \"$*\" in *'%(objectmode)'*) if [ \"$LC_ALL:$LANGUAGE\" != C:C ]; then echo 'unsupported format in another language' >&2; exit 128; fi; echo 'fatal: bad cat-file format: %(objectmode)' >&2; exit 128;; *' cat-file '*) for arg in \"$@\"; do if [ \"$arg\" = -z ] || [ \"$arg\" = -Z ]; then echo 'unsupported cat-file option' >&2; exit 129; fi; done;; esac\nexec git \"$@\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut req = request(&root);
    req.git_executable = executable.clone();
    assert!(!supports_object_mode(&req).unwrap());
    let result = metadata(
        &req,
        &[
            item(":0", "file"),
            item(":0", unusual),
            item("HEAD", unusual),
            item(&commit.to_string(), unusual),
            item(":1", unusual),
            item(":0", "missing\n[?]*"),
            item(":0", "gitlink"),
            item("HEAD", "gitlink"),
        ],
    )
    .unwrap();
    assert_eq!(result["results"][0]["metadata"]["size"], 8);
    for entry in &result["results"].as_array().unwrap()[1..=3] {
        assert_eq!(entry["metadata"]["oid"], original.to_string());
        assert_eq!(entry["metadata"]["size"], 28);
    }
    assert_eq!(result["results"][4]["error"]["code"], "content_not_found");
    assert_eq!(result["results"][5]["error"]["code"], "content_not_found");
    assert_eq!(result["results"][6]["error"]["code"], "unsupported_content");
    assert_eq!(result["results"][7]["error"]["code"], "unsupported_content");
    let template = index.get_path(Path::new(unusual), 0).unwrap();
    index.remove_path(Path::new(unusual)).unwrap();
    let conflict_oid = repo.blob(b"conflict stage contents").unwrap();
    let mut conflict = IndexEntry {
        id: conflict_oid,
        ..template
    };
    conflict.flags = (conflict.flags & !(3 << 12)) | (2 << 12);
    index.add(&conflict).unwrap();
    index.write().unwrap();
    let stages = metadata(
        &req,
        &[
            item(":2", unusual),
            item(":0", unusual),
            item(":1", unusual),
        ],
    )
    .unwrap();
    assert_eq!(
        stages["results"][0]["metadata"]["oid"],
        conflict_oid.to_string()
    );
    assert_eq!(stages["results"][0]["metadata"]["size"], 23);
    assert_eq!(stages["results"][1]["error"]["code"], "content_not_found");
    assert_eq!(stages["results"][2]["error"]["code"], "content_not_found");
    let missing_blob = index.get_path(Path::new("file"), 0).unwrap().id.to_string();
    fs::remove_file(
        root.join(".git/objects")
            .join(&missing_blob[..2])
            .join(&missing_blob[2..]),
    )
    .unwrap();
    assert_eq!(
        metadata(&req, &[item(":0", "file")]).unwrap_err().code,
        "stale_target"
    );
    fs::write(
        &executable,
        "#!/bin/sh\necho 'fatal: broken Git configuration' >&2\nexit 128\n",
    )
    .unwrap();
    let error = metadata(&req, &[item(":0", "file")]).unwrap_err();
    assert_eq!(error.code, "repository_error");
    assert!(error.message.contains("broken Git configuration"));
}

#[test]
fn fast_metadata_reads_sparse_and_split_indexes() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    if !supports_object_mode(&request(&root)).unwrap() {
        return;
    }
    for file in ["keep/a", "skip/deep/file"] {
        fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
        fs::write(root.join(file), file.as_bytes()).unwrap();
    }
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("keep/a")).unwrap();
    index.add_path(Path::new("skip/deep/file")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let items = [
        item(":0", "keep/a"),
        item(":0", "skip/deep/file"),
        item("HEAD", "skip/deep/file"),
    ];
    run_git(&root, &["update-index", "--split-index"]);
    assert_eq!(
        mode_results(&request(&root), &items).unwrap(),
        canonical_results(&request(&root), &items).unwrap()
    );
    run_git(&root, &["update-index", "--no-split-index"]);
    run_git(
        &root,
        &["sparse-checkout", "init", "--cone", "--sparse-index"],
    );
    run_git(&root, &["sparse-checkout", "set", "keep"]);
    assert_eq!(
        mode_results(&request(&root), &items).unwrap(),
        canonical_results(&request(&root), &items).unwrap()
    );
}

#[test]
fn fast_head_tracks_new_commits_while_pinned_commit_stays_fixed() {
    if !crate::git::git_test_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    if !supports_object_mode(&request(&root)).unwrap() {
        return;
    }
    fs::write(root.join("file"), b"first").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.write().unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let first = repo
        .commit(Some("HEAD"), &signature, &signature, "first", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(root.join("file"), b"second commit").unwrap();
    index.add_path(Path::new("file")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.find_commit(first).unwrap();
    let second = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "second",
            &tree,
            &[&parent],
        )
        .unwrap();
    let items = [item("HEAD", "file"), item(&first.to_string(), "file")];
    let values = mode_results(&request(&root), &items).unwrap();
    assert_eq!(values[0]["metadata"]["head_oid"], second.to_string());
    assert_eq!(values[0]["metadata"]["size"], 13);
    assert_eq!(values[1]["metadata"]["head_oid"], first.to_string());
    assert_eq!(values[1]["metadata"]["size"], 5);
    assert_eq!(values, canonical_results(&request(&root), &items).unwrap());
}
