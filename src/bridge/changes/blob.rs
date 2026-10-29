use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bridge::{BridgeError, changes::RepositoryIdentity, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct MetadataRequest {
    repo: PathBuf,
    expected_repo_identity: String,
    #[serde(rename = "ref")]
    reference: String,
    path: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct BatchRequest {
    repo: PathBuf,
    expected_repo_identity: String,
    items: Vec<BatchItem>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchItem {
    #[serde(rename = "ref")]
    reference: String,
    path: String,
}

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Git content changed; refresh and try again.",
    )
}

fn content_not_found() -> BridgeError {
    BridgeError::new("content_not_found", "Git content was not found.")
}

fn repository_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

pub(in crate::bridge) fn commit_oid(reference: &str) -> bool {
    reference.len() == 40
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_reference(reference: &str) -> bool {
    matches!(reference, "HEAD" | "" | ":1" | ":2" | ":3") || commit_oid(reference)
}

fn valid_identity(identity: &str) -> bool {
    identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(in crate::bridge) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.as_bytes().contains(&0)
        && !Path::new(path).is_absolute()
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && (!cfg!(windows) || !path.contains('\\'))
        && (!cfg!(windows) || path.as_bytes().get(1).is_none_or(|byte| *byte != b':'))
}

fn object(
    repo: &git2::Repository,
    reference: &str,
    path: &Path,
    cached_index: Option<&git2::Index>,
) -> Result<(git2::Oid, u32, Option<git2::Oid>), BridgeError> {
    if reference == "HEAD" || commit_oid(reference) {
        let commit = if reference == "HEAD" {
            let head = repo.head().map_err(|error| {
                if matches!(
                    error.code(),
                    git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
                ) {
                    content_not_found()
                } else {
                    repository_error(error)
                }
            })?;
            head.peel_to_commit().map_err(repository_error)?
        } else {
            let oid = git2::Oid::from_str(reference)
                .map_err(|_| invalid("Invalid Git content reference."))?;
            let object = repo.find_object(oid, None).map_err(|error| {
                if error.code() == git2::ErrorCode::NotFound {
                    content_not_found()
                } else {
                    repository_error(error)
                }
            })?;
            object.into_commit().map_err(|_| {
                BridgeError::new(
                    "unsupported_content",
                    "Git content reference is not a commit.",
                )
            })?
        };
        let tree = commit.tree().map_err(repository_error)?;
        let entry = tree.get_path(path).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                content_not_found()
            } else {
                repository_error(error)
            }
        })?;
        return Ok((entry.id(), entry.filemode() as u32, Some(commit.id())));
    }
    let stage = match reference {
        "" => 0,
        ":1" => 1,
        ":2" => 2,
        ":3" => 3,
        _ => return Err(invalid("Invalid Git content reference.")),
    };
    let owned_index;
    let index = if let Some(index) = cached_index {
        index
    } else {
        owned_index = read_index(repo)?;
        &owned_index
    };
    let entry = index.get_path(path, stage).ok_or_else(content_not_found)?;
    if entry.path.as_slice() != path.as_os_str().as_encoded_bytes() {
        return Err(content_not_found());
    }
    Ok((entry.id, entry.mode, None))
}

fn read_index(repo: &git2::Repository) -> Result<git2::Index, BridgeError> {
    let mut index = repo.index().map_err(repository_error)?;
    index.read(true).map_err(repository_error)?;
    Ok(index)
}

fn blob_value(
    repo: &git2::Repository,
    odb: &git2::Odb<'_>,
    reference: &str,
    path: &str,
    cached_index: Option<&git2::Index>,
) -> Result<Value, BridgeError> {
    let (oid, mode, head_oid) = object(repo, reference, Path::new(path), cached_index)?;
    if !matches!(mode & 0o170000, 0o100000 | 0o120000) {
        return Err(BridgeError::new(
            "unsupported_content",
            "Git content is not a file or symlink.",
        ));
    }
    let (size, kind) = odb.read_header(oid).map_err(repository_error)?;
    if kind != git2::ObjectType::Blob {
        return Err(BridgeError::new(
            "unsupported_content",
            "Git content is not a blob.",
        ));
    }
    Ok(json!({"oid": oid.to_string(), "mode": mode, "size": size,
        "head_oid": head_oid.map(|value| value.to_string())}))
}

pub(in crate::bridge) fn metadata(
    scope: &Scope,
    request: MetadataRequest,
) -> Result<Value, BridgeError> {
    if !valid_identity(&request.expected_repo_identity)
        || !valid_path(&request.path)
        || !valid_reference(&request.reference)
    {
        return Err(invalid("Invalid Git content request."));
    }
    let repo_path = scope.repository(&request.repo)?;
    let identity = RepositoryIdentity::read(&repo_path)?;
    if identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path).map_err(repository_error)?;
    let odb = repo.odb().map_err(repository_error)?;
    let result = blob_value(&repo, &odb, &request.reference, &request.path, None);
    // A missing entry must not mask a repository replacement while it was
    // looked up. Preserve the distinct identity failure for document guards.
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != identity
    {
        return Err(stale());
    }
    let first = result?;
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != identity
        || blob_value(&repo, &odb, &request.reference, &request.path, None).map_err(|error| {
            if error.code == "content_not_found" {
                stale()
            } else {
                error
            }
        })? != first
    {
        return Err(stale());
    }
    Ok(first)
}

fn batch_pass(
    repo: &git2::Repository,
    items: &[BatchItem],
) -> Result<Vec<Result<Value, BridgeError>>, BridgeError> {
    let index = if items
        .iter()
        .any(|item| matches!(item.reference.as_str(), "" | ":1" | ":2" | ":3"))
    {
        Some(read_index(repo).map_err(|error| error.message))
    } else {
        None
    };
    let odb = repo.odb().map_err(repository_error)?;
    Ok(items
        .iter()
        .map(|item| {
            if !valid_reference(&item.reference) || !valid_path(&item.path) {
                return Err(invalid("Invalid Git content request."));
            }
            let item_index = if matches!(item.reference.as_str(), "" | ":1" | ":2" | ":3") {
                Some(
                    index
                        .as_ref()
                        .expect("stage items load an index")
                        .as_ref()
                        .map_err(repository_error)?,
                )
            } else {
                None
            };
            blob_value(repo, &odb, &item.reference, &item.path, item_index)
        })
        .collect())
}

fn batch_result(first: Result<Value, BridgeError>, second: Result<Value, BridgeError>) -> Value {
    match (first, second) {
        (Ok(first), Ok(second)) if first == second => json!({"metadata": first}),
        (Err(first), Err(second))
            if first.code == second.code && first.message == second.message =>
        {
            json!({"error": {"code": first.code, "message": first.message}})
        }
        (Err(error), _) | (_, Err(error)) if error.code != "content_not_found" => {
            json!({"error": {"code": error.code, "message": error.message}})
        }
        _ => {
            json!({"error": {"code": "stale_target", "message": "Git content changed; refresh and try again."}})
        }
    }
}

pub(in crate::bridge) fn metadata_batch(
    scope: &Scope,
    request: BatchRequest,
) -> Result<Value, BridgeError> {
    if !valid_identity(&request.expected_repo_identity)
        || request.items.is_empty()
        || request.items.len() > 128
        || serde_json::to_vec(&request)
            .map_err(repository_error)?
            .len()
            > crate::bridge::MAX_REQUEST_BYTES as usize
    {
        return Err(invalid(
            "A valid repository identity and 1 to 128 Git content items within the request limit are required.",
        ));
    }
    let repo_path = scope.repository(&request.repo)?;
    let identity = RepositoryIdentity::read(&repo_path)?;
    if identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path).map_err(repository_error)?;
    let first = batch_pass(&repo, &request.items)?;
    let second = batch_pass(&repo, &request.items)?;
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != identity
    {
        return Err(stale());
    }
    let results: Vec<_> = first
        .into_iter()
        .zip(second)
        .map(|(first, second)| batch_result(first, second))
        .collect();
    Ok(json!({"results": results}))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use git2::{IndexEntry, Repository, Signature};
    use serde_json::{Value, json};

    use super::{
        BatchItem, BatchRequest, MetadataRequest, batch_result, content_not_found, metadata,
        metadata_batch,
    };
    use crate::bridge::{changes::RepositoryIdentity, scope::Scope};

    fn fixture() -> (tempfile::TempDir, PathBuf, Repository) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let repo = Repository::init(&root).unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        for (path, contents) in [
            ("docs/file[1].txt", b"exact\0binary".as_slice()),
            ("docs/file1.txt", b"wildcard".as_slice()),
        ] {
            fs::write(root.join(path), contents).unwrap();
        }
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("docs/file[1].txt")).unwrap();
        index.add_path(Path::new("docs/file1.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);
        drop(index);
        (directory, root, repo)
    }

    fn request(root: &Path, reference: &str, path: &str) -> MetadataRequest {
        MetadataRequest {
            repo: root.to_path_buf(),
            expected_repo_identity: RepositoryIdentity::read(root)
                .unwrap()
                .fingerprint()
                .unwrap(),
            reference: reference.to_owned(),
            path: path.to_owned(),
        }
    }

    fn batch(root: &Path, items: &[(&str, &str)]) -> BatchRequest {
        BatchRequest {
            repo: root.to_path_buf(),
            expected_repo_identity: RepositoryIdentity::read(root)
                .unwrap()
                .fingerprint()
                .unwrap(),
            items: items
                .iter()
                .map(|(reference, path)| BatchItem {
                    reference: (*reference).to_owned(),
                    path: (*path).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn exact_head_and_index_paths_resolve_despite_a_blocking_file() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let head_oid = repo.head().unwrap().target().unwrap();
        let exact_oid = repo
            .index()
            .unwrap()
            .get_path(Path::new("docs/file[1].txt"), 0)
            .unwrap()
            .id;
        fs::remove_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs"), "blocking file").unwrap();
        for reference in ["HEAD", ""] {
            let result = metadata(&scope, request(&root, reference, "docs/file[1].txt")).unwrap();
            assert_eq!(result["oid"], exact_oid.to_string());
            assert_eq!(result["mode"], 0o100644);
            assert_eq!(result["size"], 12);
            assert_eq!(
                result["head_oid"],
                if reference == "HEAD" {
                    json!(head_oid.to_string())
                } else {
                    Value::Null
                }
            );
        }
    }

    #[test]
    fn metadata_tracks_staged_contents_and_exact_conflict_stage() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let path = Path::new("docs/file[1].txt");
        let mut index = repo.index().unwrap();
        let template = index.get_path(path, 0).unwrap();
        let staged = repo.blob(b"new staged contents").unwrap();
        let mut changed = IndexEntry {
            id: staged,
            ..template
        };
        index.add(&changed).unwrap();
        index.write().unwrap();
        let value = metadata(&scope, request(&root, "", "docs/file[1].txt")).unwrap();
        assert_eq!(value["oid"], staged.to_string());
        assert_eq!(value["size"], 19);
        assert!(value["head_oid"].is_null());

        index.remove_path(path).unwrap();
        for (stage, contents) in [(1, b"base".as_slice()), (3, b"theirs".as_slice())] {
            changed.id = repo.blob(contents).unwrap();
            changed.flags = (changed.flags & !(3 << 12)) | (stage << 12);
            index.add(&changed).unwrap();
        }
        index.write().unwrap();
        assert_eq!(
            metadata(&scope, request(&root, ":1", "docs/file[1].txt")).unwrap()["size"],
            4
        );
        assert_eq!(
            metadata(&scope, request(&root, ":3", "docs/file[1].txt")).unwrap()["size"],
            6
        );
        for missing in ["", ":2"] {
            assert_eq!(
                metadata(&scope, request(&root, missing, "docs/file[1].txt"))
                    .unwrap_err()
                    .code,
                "content_not_found"
            );
        }
    }

    #[test]
    fn batch_returns_ordered_exact_metadata_and_individual_errors() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let commit = repo.head().unwrap().target().unwrap().to_string();
        let output = metadata_batch(
            &scope,
            batch(
                &root,
                &[
                    ("HEAD", "docs/file[1].txt"),
                    ("", "docs/file[1].txt"),
                    (&commit, "docs/file1.txt"),
                    ("HEAD", "docs/file[1].txt"),
                    (":2", "docs/file1.txt"),
                    ("HEAD~1", "docs/file1.txt"),
                    ("HEAD", "../outside"),
                    ("HEAD", "docs"),
                ],
            ),
        )
        .unwrap();
        let results = output["results"].as_array().unwrap();
        assert_eq!(results.len(), 8);
        assert_eq!(results[0]["metadata"]["size"], 12);
        assert_eq!(results[1]["metadata"]["size"], 12);
        assert_eq!(results[2]["metadata"]["size"], 8);
        assert_eq!(results[2]["metadata"]["head_oid"], commit);
        assert_eq!(results[3], results[0]);
        assert_eq!(results[4]["error"]["code"], "content_not_found");
        assert_eq!(results[5]["error"]["code"], "invalid_request");
        assert_eq!(results[6]["error"]["code"], "invalid_request");
        assert_eq!(results[7]["error"]["code"], "unsupported_content");
    }

    #[test]
    fn batch_enforces_scope_identity_count_and_byte_limits() {
        let (_directory, root, _repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        assert_eq!(
            metadata_batch(&scope, batch(&root, &[])).unwrap_err().code,
            "invalid_request"
        );
        assert!(
            metadata_batch(&scope, batch(&root, &vec![("HEAD", "docs/file1.txt"); 128])).is_ok()
        );
        assert_eq!(
            metadata_batch(&scope, batch(&root, &vec![("HEAD", "docs/file1.txt"); 129]))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        let huge = "a".repeat(crate::bridge::MAX_REQUEST_BYTES as usize);
        assert_eq!(
            metadata_batch(&scope, batch(&root, &[("HEAD", &huge)]))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        let mut changed = batch(&root, &[("HEAD", "docs/file1.txt")]);
        changed.expected_repo_identity = "1".repeat(64);
        assert_eq!(
            metadata_batch(&scope, changed).unwrap_err().code,
            "stale_target"
        );
        let outside = tempfile::tempdir().unwrap();
        let outside_scope = Scope::new(vec![outside.path().canonicalize().unwrap()]).unwrap();
        assert_eq!(
            metadata_batch(&outside_scope, batch(&root, &[("HEAD", "docs/file1.txt")]))
                .unwrap_err()
                .code,
            "outside_workspace"
        );
    }

    #[test]
    fn batch_reads_exact_conflict_stages_without_aliasing_missing_sides() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let path = Path::new("docs/file[1].txt");
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(path, 0).unwrap();
        index.remove_path(path).unwrap();
        for (stage, contents) in [(1, b"base".as_slice()), (3, b"theirs".as_slice())] {
            entry.id = repo.blob(contents).unwrap();
            entry.flags = (entry.flags & !(3 << 12)) | (stage << 12);
            index.add(&entry).unwrap();
        }
        index.write().unwrap();
        let output = metadata_batch(
            &scope,
            batch(
                &root,
                &[
                    (":1", "docs/file[1].txt"),
                    (":2", "docs/file[1].txt"),
                    (":3", "docs/file[1].txt"),
                    ("", "docs/file[1].txt"),
                ],
            ),
        )
        .unwrap();
        let results = output["results"].as_array().unwrap();
        assert_eq!(results[0]["metadata"]["size"], 4);
        assert_eq!(results[1]["error"]["code"], "content_not_found");
        assert_eq!(results[2]["metadata"]["size"], 6);
        assert_eq!(results[3]["error"]["code"], "content_not_found");
    }

    #[test]
    fn batch_detects_objects_appearing_disappearing_or_changing_during_a_scan() {
        let first = json!({"oid": "first", "mode": 0o100644, "size": 1, "head_oid": null});
        let changed = json!({"oid": "changed", "mode": 0o100644, "size": 2, "head_oid": null});
        for result in [
            batch_result(Ok(first.clone()), Ok(changed)),
            batch_result(Ok(first.clone()), Err(content_not_found())),
            batch_result(Err(content_not_found()), Ok(first)),
        ] {
            assert_eq!(result["error"]["code"], "stale_target");
        }
        assert_eq!(
            batch_result(Err(content_not_found()), Err(content_not_found()))["error"]["code"],
            "content_not_found"
        );
    }

    #[test]
    fn immutable_commit_content_survives_head_changes() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let original = repo.head().unwrap().peel_to_commit().unwrap();
        let original_oid = original.id().to_string();
        let old = metadata(&scope, request(&root, &original_oid, "docs/file[1].txt")).unwrap();

        fs::write(root.join("docs/file[1].txt"), b"new commit").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("docs/file[1].txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::now("Test", "test@example.invalid").unwrap();
        let next = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "next",
                &tree,
                &[&original],
            )
            .unwrap();

        let fixed = metadata(&scope, request(&root, &original_oid, "docs/file[1].txt")).unwrap();
        let head = metadata(&scope, request(&root, "HEAD", "docs/file[1].txt")).unwrap();
        assert_eq!(fixed, old);
        assert_eq!(fixed["head_oid"], original_oid);
        assert_eq!(head["head_oid"], next.to_string());
        assert_ne!(head["oid"], fixed["oid"]);
    }

    #[test]
    fn absent_index_entries_can_appear_and_disappear_without_masking_identity_changes() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let path = "docs/new[1].txt";
        assert_eq!(
            metadata(&scope, request(&root, "", path)).unwrap_err().code,
            "content_not_found"
        );
        fs::write(root.join(path), b"new content").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        assert_eq!(
            metadata(&scope, request(&root, "", path)).unwrap()["size"],
            11
        );
        index.remove_path(Path::new(path)).unwrap();
        index.write().unwrap();
        assert_eq!(
            metadata(&scope, request(&root, "", path)).unwrap_err().code,
            "content_not_found"
        );

        let mut changed = request(&root, "", path);
        changed.expected_repo_identity = "1".repeat(64);
        assert_eq!(metadata(&scope, changed).unwrap_err().code, "stale_target");
    }

    #[test]
    fn missing_and_non_commit_refs_have_distinct_errors() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        let missing = "0".repeat(40);
        assert_eq!(
            metadata(&scope, request(&root, &missing, "docs/file1.txt"))
                .unwrap_err()
                .code,
            "content_not_found"
        );
        let blob = repo.blob(b"not a commit").unwrap().to_string();
        assert_eq!(
            metadata(&scope, request(&root, &blob, "docs/file1.txt"))
                .unwrap_err()
                .code,
            "unsupported_content"
        );
        for invalid in ["head", "HEAD~1", "1234", &"A".repeat(40), &"a".repeat(41)] {
            assert_eq!(
                metadata(&scope, request(&root, invalid, "docs/file1.txt"))
                    .unwrap_err()
                    .code,
                "invalid_request"
            );
        }
        assert_eq!(
            metadata(&scope, request(&root, "HEAD", "docs/absent"))
                .unwrap_err()
                .code,
            "content_not_found"
        );

        let unborn_dir = tempfile::tempdir().unwrap();
        let unborn = unborn_dir.path().canonicalize().unwrap();
        Repository::init(&unborn).unwrap();
        let unborn_scope = Scope::new(vec![unborn.clone()]).unwrap();
        for reference in ["HEAD", "", ":2"] {
            assert_eq!(
                metadata(&unborn_scope, request(&unborn, reference, "absent"))
                    .unwrap_err()
                    .code,
                "content_not_found"
            );
        }
    }

    #[test]
    fn rejects_unsafe_paths_invalid_refs_and_changed_repository_identity() {
        let (_directory, root, _repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        for path in [
            "",
            "/etc/passwd",
            "../file",
            "docs/../file",
            "docs/./file",
            "docs//file",
            "docs/file\0name",
        ] {
            assert_eq!(
                metadata(&scope, request(&root, "HEAD", path))
                    .unwrap_err()
                    .code,
                "invalid_request"
            );
        }
        assert_eq!(
            metadata(&scope, request(&root, ":4", "docs/file1.txt"))
                .unwrap_err()
                .code,
            "invalid_request"
        );
        let mut changed = request(&root, "HEAD", "docs/file1.txt");
        changed.expected_repo_identity = "1".repeat(64);
        assert_eq!(metadata(&scope, changed).unwrap_err().code, "stale_target");
        let outside = tempfile::tempdir().unwrap();
        let outside_scope = Scope::new(vec![outside.path().canonicalize().unwrap()]).unwrap();
        assert_eq!(
            metadata(&outside_scope, request(&root, "HEAD", "docs/file1.txt"))
                .unwrap_err()
                .code,
            "outside_workspace"
        );
    }

    #[test]
    fn rejects_tree_entries_and_reports_large_binary_blob_size() {
        let (_directory, root, repo) = fixture();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        assert_eq!(
            metadata(&scope, request(&root, "HEAD", "docs"))
                .unwrap_err()
                .code,
            "unsupported_content"
        );
        let large = vec![0xff; 2 * 1024 * 1024];
        fs::write(root.join("large"), &large).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("large")).unwrap();
        index.write().unwrap();
        assert_eq!(
            metadata(&scope, request(&root, "", "large")).unwrap()["size"],
            large.len()
        );
    }

    #[test]
    fn refuses_case_insensitive_index_aliases() {
        let (_directory, root, repo) = fixture();
        repo.config()
            .unwrap()
            .set_bool("core.ignorecase", true)
            .unwrap();
        let scope = Scope::new(vec![root.clone()]).unwrap();
        assert_eq!(
            metadata(&scope, request(&root, "", "docs/FILE1.txt"))
                .unwrap_err()
                .code,
            "content_not_found"
        );
    }
}
