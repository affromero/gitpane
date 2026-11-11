use std::fs;
use std::io::Cursor;
use std::path::Path;

use git2::{Oid, Repository};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::bridge::changes::RepositoryIdentity;
use crate::config::{GraphConfig, SubmoduleConfig, WorktreeConfig};
use crate::git::stash::test_support::{commit, files, stash, tree};

fn fixture() -> (TempDir, Repository, String) {
    let directory = TempDir::new().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let original = repo.blob(b"renamed contents\n").unwrap();
    let deleted = repo.blob(b"deleted contents\n").unwrap();
    let old_binary = repo.blob(b"old\0bytes").unwrap();
    let new_binary = repo.blob(b"new\0bytes").unwrap();
    let pointer = |digit: &str| Oid::from_str(&digit.repeat(40)).unwrap();
    let base_tree = tree(
        &repo,
        &[
            ("old name.txt", original, 0o100644),
            ("deleted.txt", deleted, 0o100644),
            ("binary.dat", old_binary, 0o100644),
            ("module", pointer("1"), 0o160000),
        ],
    );
    let base = commit(&repo, base_tree, &[], "base");
    repo.reference("refs/heads/main", base, true, "base")
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let saved = tree(
        &repo,
        &[
            ("new name.txt", original, 0o100644),
            ("binary.dat", new_binary, 0o100644),
            ("module", pointer("2"), 0o160000),
        ],
    );
    let untracked = files(&repo, &[("nested/saved.txt", b"saved untracked\n")]);
    let oid = stash(&repo, base, saved, saved, Some(untracked)).to_string();
    (directory, repo, oid)
}

fn exchange(root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut input = serde_json::to_vec(&json!({
        "id": 0,
        "method": "initialize",
        "params": {"version": 1, "workspace_roots": [root]},
    }))
    .unwrap();
    input.push(b'\n');
    for request in requests {
        serde_json::to_writer(&mut input, request).unwrap();
        input.push(b'\n');
    }
    let mut output = Vec::new();
    crate::bridge::serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        &GraphConfig::default(),
        &WorktreeConfig::default(),
        None,
        false,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn content_request(root: &Path, oid: &str, source: &str, path: &str) -> Value {
    json!({
        "id": 2,
        "method": "stash_file_content",
        "params": {
            "repo": root,
            "oid": oid,
            "expected_repo_identity": RepositoryIdentity::read(root).unwrap().fingerprint().unwrap(),
            "source": source,
            "path": path,
        },
    })
}

#[test]
fn routes_return_stable_file_dtos_and_saved_untracked_content() {
    let (directory, _repo, oid) = fixture();
    let root = directory.path();
    let replies = exchange(
        root,
        &[
            json!({"id": 1, "method": "stash_files", "params": {"repo": root, "oid": oid}}),
            content_request(root, &oid, "saved_untracked", "nested/saved.txt"),
        ],
    );
    let listing = &replies[1]["result"];
    assert_eq!(replies[1]["id"], 1);
    assert_eq!(listing["oid"], oid);
    assert_eq!(listing["message"], "On main: saved snapshot");
    assert_eq!(listing["parent_oid"].as_str().unwrap().len(), 40);
    assert_eq!(listing["untracked_parent_oid"].as_str().unwrap().len(), 40);
    assert_eq!(listing["repo_identity"].as_str().unwrap().len(), 64);
    let files = listing["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    let saved = files
        .iter()
        .find(|file| file["path"] == "nested/saved.txt")
        .unwrap();
    assert_eq!(
        saved,
        &json!({
            "source": "saved_untracked", "status": "A", "path": "nested/saved.txt",
            "old_path": null, "new_path": "nested/saved.txt", "is_binary": false, "is_submodule": false,
        })
    );
    let content = &replies[2]["result"];
    assert_eq!(content["file"], *saved);
    assert_eq!(content["oid"], oid);
    assert_eq!(content["repo_identity"], listing["repo_identity"]);
    assert_eq!(
        content["old"],
        json!({"kind": "missing", "oid": null, "mode": null, "size": 0, "text": ""})
    );
    assert_eq!(content["new"]["kind"], "text");
    assert_eq!(content["new"]["mode"], 0o100644);
    assert_eq!(content["new"]["text"], "saved untracked\n");
    assert!(
        content["diff"]
            .as_str()
            .unwrap()
            .contains("+saved untracked\n")
    );
}

#[test]
fn renamed_deleted_binary_and_gitlink_responses_have_explicit_sides() {
    let (directory, _repo, oid) = fixture();
    let root = directory.path();
    let replies = exchange(
        root,
        &[
            content_request(root, &oid, "tracked", "new name.txt"),
            content_request(root, &oid, "tracked", "deleted.txt"),
            content_request(root, &oid, "tracked", "binary.dat"),
            content_request(root, &oid, "tracked", "module"),
        ],
    );
    let renamed = &replies[1]["result"];
    assert_eq!(renamed["file"]["status"], "R");
    assert_eq!(renamed["file"]["old_path"], "old name.txt");
    assert_eq!(renamed["file"]["new_path"], "new name.txt");
    assert_eq!(renamed["old"]["text"], renamed["new"]["text"]);
    let deleted = &replies[2]["result"];
    assert_eq!(deleted["file"]["status"], "D");
    assert!(deleted["file"]["new_path"].is_null());
    assert_eq!(deleted["old"]["text"], "deleted contents\n");
    assert_eq!(deleted["new"]["kind"], "missing");
    assert_eq!(deleted["new"]["text"], "");
    let binary = &replies[3]["result"];
    assert_eq!(binary["file"]["is_binary"], true);
    assert_eq!(binary["old"]["kind"], "binary");
    assert_eq!(binary["new"]["kind"], "binary");
    assert!(binary["old"]["text"].is_null());
    assert!(binary["new"]["text"].is_null());
    assert!(
        binary["diff"]
            .as_str()
            .unwrap()
            .contains("Binary files differ")
    );
    let gitlink = &replies[4]["result"];
    assert_eq!(gitlink["file"]["is_submodule"], true);
    assert_eq!(gitlink["old"]["kind"], "gitlink");
    assert_eq!(gitlink["new"]["mode"], 0o160000);
    assert_eq!(
        gitlink["new"]["text"],
        format!("Subproject commit {}\n", "2".repeat(40))
    );
}

#[test]
fn full_oid_source_identity_and_literal_path_are_validated_at_the_route() {
    let (directory, _repo, oid) = fixture();
    let root = directory.path();
    let valid = content_request(root, &oid, "saved_untracked", "nested/saved.txt");
    let mut invalid = Vec::new();
    for field in ["oid", "source", "expected_repo_identity", "path"] {
        let mut request = valid.clone();
        request["params"][field] = json!(match field {
            "oid" => "stash@{0}",
            "source" => "worktree",
            "expected_repo_identity" => "bad",
            "path" => "../outside",
            _ => unreachable!(),
        });
        invalid.push(request);
    }
    let mut unknown = valid.clone();
    unknown["params"]["old_path"] = json!("unexpected");
    invalid.push(unknown);
    invalid.push(
        json!({"id": 3, "method": "stash_files", "params": {"repo": root, "oid": &oid[..12]}}),
    );
    for reply in exchange(root, &invalid).iter().skip(1) {
        assert_eq!(reply["error"]["code"], "invalid_request", "{reply}");
    }
    let mut stale = valid.clone();
    stale["params"]["expected_repo_identity"] = json!("0".repeat(64));
    assert_eq!(exchange(root, &[stale])[1]["error"]["code"], "stale_target");
    let missing =
        json!({"id": 4, "method": "stash_files", "params": {"repo": root, "oid": "f".repeat(40)}});
    assert_eq!(
        exchange(root, &[missing])[1]["error"]["code"],
        "content_not_found"
    );
}

#[test]
fn requests_cannot_read_repositories_outside_the_initialized_workspace() {
    let (workspace, _repo, _oid) = fixture();
    let (outside, _outside_repo, outside_oid) = fixture();
    let requests = [
        json!({"id": 1, "method": "stash_files", "params": {"repo": outside.path(), "oid": outside_oid}}),
        content_request(outside.path(), &outside_oid, "tracked", "module"),
    ];
    let replies = exchange(workspace.path(), &requests);
    for reply in replies.iter().skip(1) {
        assert_eq!(reply["error"]["code"], "outside_workspace");
    }
}

#[test]
fn route_reads_saved_objects_even_with_unrelated_worktree_and_corrupt_index() {
    let (directory, repo, oid) = fixture();
    let root = directory.path();
    let requests = [
        json!({"id": 1, "method": "stash_files", "params": {"repo": root, "oid": oid}}),
        content_request(root, &oid, "tracked", "new name.txt"),
    ];
    let expected = exchange(root, &requests);
    fs::write(root.join("new name.txt"), b"current worktree\n").unwrap();
    fs::write(root.join(".gitattributes"), b"* binary\n").unwrap();
    fs::write(repo.path().join("index"), b"invalid current index").unwrap();
    let head = fs::read(repo.path().join("HEAD")).unwrap();
    assert_eq!(exchange(root, &requests), expected);
    assert_eq!(
        fs::read(root.join("new name.txt")).unwrap(),
        b"current worktree\n"
    );
    assert_eq!(
        fs::read(repo.path().join("index")).unwrap(),
        b"invalid current index"
    );
    assert_eq!(fs::read(repo.path().join("HEAD")).unwrap(), head);
}

#[cfg(unix)]
#[test]
fn bridge_keeps_pathspec_magic_and_newlines_literal() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let repo = Repository::init(root).unwrap();
    let empty = files(&repo, &[]);
    let base = commit(&repo, empty, &[], "base");
    let name = ":(glob)file[*]\n'\\.txt";
    let saved = files(&repo, &[(name, b"literal path contents\n")]);
    let oid = stash(&repo, base, empty, empty, Some(saved)).to_string();
    let replies = exchange(
        root,
        &[
            json!({"id": 1, "method": "stash_files", "params": {"repo": root, "oid": oid}}),
            content_request(root, &oid, "saved_untracked", name),
        ],
    );
    assert_eq!(replies[1]["result"]["files"][0]["path"], name);
    assert_eq!(
        replies[2]["result"]["new"]["text"],
        "literal path contents\n"
    );
}
