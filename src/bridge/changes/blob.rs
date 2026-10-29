use std::path::{Path, PathBuf};

use serde::Deserialize;
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

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Git content changed; refresh and try again.",
    )
}

fn repository_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn valid_path(path: &str) -> bool {
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
) -> Result<(git2::Oid, u32, Option<git2::Oid>), BridgeError> {
    if reference == "HEAD" {
        let head = repo.head().map_err(|error| {
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) {
                stale()
            } else {
                repository_error(error)
            }
        })?;
        let commit = head.peel_to_commit().map_err(repository_error)?;
        let tree = commit.tree().map_err(repository_error)?;
        let entry = tree.get_path(path).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                stale()
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
    let mut index = repo.index().map_err(repository_error)?;
    index.read(true).map_err(repository_error)?;
    let entry = index.get_path(path, stage).ok_or_else(stale)?;
    if entry.path.as_slice() != path.as_os_str().as_encoded_bytes() {
        return Err(stale());
    }
    Ok((entry.id, entry.mode, None))
}

pub(in crate::bridge) fn metadata(
    scope: &Scope,
    request: MetadataRequest,
) -> Result<Value, BridgeError> {
    if request.expected_repo_identity.len() != 64
        || !request
            .expected_repo_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_path(&request.path)
        || !matches!(request.reference.as_str(), "HEAD" | "" | ":1" | ":2" | ":3")
    {
        return Err(invalid("Invalid Git content request."));
    }
    let repo_path = scope.repository(&request.repo)?;
    let identity = RepositoryIdentity::read(&repo_path)?;
    if identity.fingerprint()? != request.expected_repo_identity {
        return Err(stale());
    }
    let repo = git2::Repository::open(&repo_path).map_err(repository_error)?;
    let target = Path::new(&request.path);
    let (oid, mode, head_oid) = object(&repo, &request.reference, target)?;
    if !matches!(mode & 0o170000, 0o100000 | 0o120000) {
        return Err(BridgeError::new(
            "unsupported_content",
            "Git content is not a file or symlink.",
        ));
    }
    let (size, kind) = repo
        .odb()
        .map_err(repository_error)?
        .read_header(oid)
        .map_err(repository_error)?;
    if kind != git2::ObjectType::Blob {
        return Err(BridgeError::new(
            "unsupported_content",
            "Git content is not a blob.",
        ));
    }
    if scope.repository(&request.repo)? != repo_path
        || RepositoryIdentity::read(&repo_path)? != identity
        || object(&repo, &request.reference, target)? != (oid, mode, head_oid)
    {
        return Err(stale());
    }
    Ok(json!({"oid": oid.to_string(), "mode": mode, "size": size,
        "head_oid": head_oid.map(|value| value.to_string())}))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use git2::{IndexEntry, Repository, Signature};
    use serde_json::{Value, json};

    use super::{MetadataRequest, metadata};
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
                "stale_target"
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
            "stale_target"
        );
    }
}
