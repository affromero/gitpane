use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use crate::config::SubmoduleConfig;
use crate::git::status::{
    FileEntry, FileStatus, query_status_scoped, validate_initialized_submodule, watched_file,
    watched_symlinks,
};

use super::{BridgeError, changes::RepositoryIdentity, mutation::validate_file_path, scope::Scope};

fn utf8_path(path: &Path) -> Result<&str, BridgeError> {
    path.to_str()
        .ok_or_else(|| BridgeError::new("repository_error", "repository contains a non-UTF-8 path"))
}

fn file_json(file: &FileEntry) -> Result<Value, BridgeError> {
    let status = match file.status {
        FileStatus::Modified => "modified",
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Untracked => "untracked",
        FileStatus::Conflicted => "conflicted",
    };
    Ok(json!({
        "path": utf8_path(&file.path)?,
        "status": status,
        "staged": file.staged,
        "unstaged": file.unstaged,
        "is_submodule": file.is_submodule,
    }))
}

pub(super) fn query(
    repo: &Path,
    scope: &Scope,
    sub_cfg: &SubmoduleConfig,
    include_worktrees: bool,
    summary_only: bool,
    watched_paths: &[PathBuf],
) -> Result<Value, BridgeError> {
    if scope.repository(repo)? != repo {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while collecting status",
        ));
    }
    let repo_identity = RepositoryIdentity::read(repo)?;
    if watched_paths.len() > 256
        || watched_paths.iter().any(|path| {
            let bytes = path.as_os_str().as_encoded_bytes();
            bytes.is_empty()
                || bytes.contains(&0)
                || bytes
                    .last()
                    .is_some_and(|byte| *byte == b'/' || (cfg!(windows) && *byte == b'\\'))
                || path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
        })
    {
        return Err(BridgeError::new(
            "invalid_request",
            "watched paths must be relative file paths, with at most 256 entries",
        ));
    }
    let watched: HashSet<&Path> = watched_paths.iter().map(PathBuf::as_path).collect();
    if !summary_only {
        for path in &watched {
            validate_file_path(scope, repo, path)?;
        }
    }
    validate_submodules(scope, repo, sub_cfg)?;
    let common_repo = git2::Repository::open(repo)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let common_dir = common_repo
        .commondir()
        .canonicalize()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let failure = RefCell::new(None);
    let worktree_identities = RefCell::new(HashMap::<PathBuf, String>::new());
    let allow_worktree = |path: &Path| match scope.allows_missing(path) {
        Ok(false) => false,
        Ok(true) => match scope.repository(path).and_then(|path| {
            validate_submodules(scope, &path, sub_cfg)?;
            let identity = RepositoryIdentity::read(&path)?.fingerprint()?;
            worktree_identities.borrow_mut().insert(path, identity);
            Ok(())
        }) {
            Ok(()) => true,
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                false
            }
        },
        Err(error) => {
            *failure.borrow_mut() = Some(error);
            false
        }
    };
    let result = query_status_scoped(repo, sub_cfg, &allow_worktree, include_worktrees);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    let result = result.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;

    let mut files = Vec::new();
    let mut matched = HashMap::new();
    let mut included_compact = 0usize;
    for file in &result.files {
        // Validate every path even when the compact response omits it.
        utf8_path(&file.path)?;
        if !summary_only {
            files.push(file_json(file)?);
            included_compact += 1;
        } else if watched.contains(file.path.as_path()) {
            matched.insert(file.path.as_path(), file);
        }
    }
    let mut watched_errors = Vec::new();
    if summary_only {
        // Compact status groups untracked directories. Resolve watched files
        // inside those directories so an open link notices ignore or index
        // transitions even while a sibling stays untracked.
        let mut paths: Vec<_> = watched.into_iter().collect();
        paths.sort_unstable();
        let mut initial_errors = HashMap::new();
        let mut symlinks = Vec::new();
        for path in &paths {
            let inspection = validate_file_path(scope, repo, path).and_then(|()| {
                if matched.contains_key(path) {
                    return Ok(false);
                }
                match std::fs::symlink_metadata(repo.join(path)) {
                    Ok(metadata) => Ok(metadata.file_type().is_symlink()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(BridgeError::new("repository_error", error.to_string())),
                }
            });
            match inspection {
                Ok(true) => symlinks.push(*path),
                Ok(false) => {}
                Err(error) => {
                    initial_errors.insert(*path, error);
                }
            }
        }
        let symlink_set: HashSet<&Path> = symlinks.iter().copied().collect();
        let batch = watched_symlinks(&common_repo, &symlinks);
        for path in paths {
            let entry = (|| {
                if let Some(error) = initial_errors.remove(path) {
                    return Err(error);
                }
                validate_file_path(scope, repo, path)?;
                let compact_file = matched.get(path).copied();
                let exact_file = if compact_file.is_none() {
                    if symlink_set.contains(path) {
                        let batch_status = batch.as_ref().map_err(|error| {
                            BridgeError::new("repository_error", error.to_string())
                        })?;
                        if batch_status.fallback.contains(path) {
                            watched_file(&common_repo, path).map_err(|error| {
                                BridgeError::new("repository_error", error.to_string())
                            })?
                        } else {
                            batch_status.files.get(path).cloned().flatten()
                        }
                    } else {
                        watched_file(&common_repo, path).map_err(|error| {
                            BridgeError::new("repository_error", error.to_string())
                        })?
                    }
                } else {
                    None
                };
                validate_file_path(scope, repo, path)?;
                let file = compact_file.or(exact_file.as_ref());
                file.map(file_json)
                    .transpose()
                    .map(|value| (value, compact_file.is_some()))
            })();
            match entry {
                Ok((Some(value), compact)) => {
                    files.push(value);
                    included_compact += usize::from(compact);
                }
                Ok((None, _)) => {}
                Err(error) => watched_errors.push(json!({
                    "path": utf8_path(path)?,
                    "code": error.code,
                    "message": error.message,
                })),
            }
        }
    }
    let worktree_identities = worktree_identities.into_inner();
    let worktrees = result
        .worktree_info
        .iter()
        .map(|worktree| {
            let path = scope.repository(&worktree.path)?;
            let expected = worktree_identities.get(&path).ok_or_else(|| {
                BridgeError::new("stale_target", "Worktree changed while collecting status")
            })?;
            if RepositoryIdentity::read(&path)?.fingerprint()? != *expected {
                return Err(BridgeError::new(
                    "stale_target",
                    "Worktree changed while collecting status",
                ));
            }
            Ok(json!({
                "path": utf8_path(&worktree.path)?,
                "repo_identity": expected,
                "branch": worktree.branch,
                "ahead": worktree.ahead,
                "behind": worktree.behind,
                "is_dirty": worktree.is_dirty,
                "file_count": worktree.file_count,
            }))
        })
        .collect::<Result<Vec<_>, BridgeError>>()?;
    let stashes: Vec<_> = result
        .stashes
        .iter()
        .map(|stash| json!({ "index": stash.index, "message": stash.message, "oid": stash.oid }))
        .collect();
    let files_truncated = summary_only && included_compact < result.files.len();
    let mut response = json!({
        "repo_identity": repo_identity.fingerprint()?,
        "common_dir": utf8_path(&common_dir)?,
        "branch": result.branch,
        "head_oid": result.head_oid,
        "ahead": result.ahead,
        "behind": result.behind,
        "is_dirty": result.is_dirty,
        "files": files,
        "worktrees": worktrees,
        "has_submodules": result.has_submodules,
        "has_dirty_submodules": result.has_dirty_submodules,
        "has_unpushed_submodules": result.has_unpushed_submodules,
        "stashes": stashes,
    });
    if summary_only {
        response["file_count_lower_bound"] = json!(result.file_count_lower_bound);
        response["files_truncated"] = json!(files_truncated);
        response["watched_errors"] = json!(watched_errors);
    }
    if scope.repository(repo)? != repo || RepositoryIdentity::read(repo)? != repo_identity {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while collecting status",
        ));
    }
    Ok(response)
}

fn validate_submodules(
    scope: &Scope,
    repo: &Path,
    sub_cfg: &SubmoduleConfig,
) -> Result<(), BridgeError> {
    for child in scope.submodule_repositories(repo)? {
        validate_initialized_submodule(&child, sub_cfg).map_err(|error| {
            BridgeError::new(
                "repository_error",
                format!("submodule {}: {error}", child.display()),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use git2::Repository;
    use serde_json::Value;
    use tempfile::TempDir;

    use crate::bridge::scope::Scope;
    use crate::config::SubmoduleConfig;

    use super::query;

    fn fixture() -> (TempDir, PathBuf, Scope) {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        Repository::init(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let scope = Scope::new(vec![repo.clone()]).unwrap();
        (temp, repo, scope)
    }

    fn status(repo: &Path, scope: &Scope, watched: &[PathBuf]) -> Value {
        query(
            repo,
            scope,
            &SubmoduleConfig::default(),
            false,
            true,
            watched,
        )
        .unwrap()
    }

    #[test]
    fn status_fingerprints_linked_worktree_gitdir_replacement() {
        if !crate::git::git_test_available() {
            return;
        }
        fn copy_directory(source: &Path, destination: &Path) {
            std::fs::create_dir(destination).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let destination = destination.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_directory(&entry.path(), &destination);
                } else {
                    std::fs::copy(entry.path(), destination).unwrap();
                }
            }
        }
        let (temp, repo, scope) = fixture();
        let repository = Repository::open(&repo).unwrap();
        let mut index = repository.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        drop(repository);
        let linked = repo.join("linked");
        let output = crate::git::process::git_command(&repo)
            .args(["worktree", "add", "-b", "linked"])
            .arg(&linked)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let query_full =
            || query(&repo, &scope, &SubmoduleConfig::default(), true, false, &[]).unwrap();
        let before = query_full();
        let before_linked = query(
            &linked,
            &scope,
            &SubmoduleConfig::default(),
            false,
            true,
            &[],
        )
        .unwrap();
        assert_eq!(
            before["worktrees"][0]["repo_identity"],
            before_linked["repo_identity"]
        );
        let linked_repo = Repository::open(&linked).unwrap();
        let head = linked_repo.head().unwrap().target().unwrap();
        let gitdir = linked_repo.path().canonicalize().unwrap();
        let common = linked_repo.commondir().canonicalize().unwrap();
        drop(linked_repo);
        let backup = temp.path().join("old-gitdir");
        std::fs::rename(&gitdir, &backup).unwrap();
        copy_directory(&backup, &gitdir);
        let replacement = Repository::open(&linked).unwrap();
        assert_eq!(replacement.head().unwrap().target(), Some(head));
        assert_eq!(replacement.commondir().canonicalize().unwrap(), common);
        drop(replacement);
        let after = query_full();
        let after_linked = query(
            &linked,
            &scope,
            &SubmoduleConfig::default(),
            false,
            true,
            &[],
        )
        .unwrap();
        assert_eq!(after["repo_identity"], before["repo_identity"]);
        assert_eq!(
            after["worktrees"][0]["repo_identity"],
            after_linked["repo_identity"]
        );
        assert_ne!(
            after_linked["repo_identity"],
            before_linked["repo_identity"]
        );
        assert_eq!(after_linked["repo_identity"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn compact_status_reports_counts_and_only_watched_entries() {
        let (_temp, repo, scope) = fixture();
        std::fs::write(repo.join("first"), "first").unwrap();
        std::fs::write(repo.join("second"), "second").unwrap();
        let compact = status(&repo, &scope, &[PathBuf::from("first")]);
        assert_eq!(compact["file_count_lower_bound"], 2);
        assert_eq!(compact["files_truncated"], true);
        assert_eq!(compact["files"].as_array().unwrap().len(), 1);
        assert_eq!(compact["files"][0]["path"], "first");
        assert_eq!(compact["files"][0]["status"], "untracked");
        let legacy = query(
            &repo,
            &scope,
            &SubmoduleConfig::default(),
            false,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(legacy["files"].as_array().unwrap().len(), 2);
        assert!(legacy.get("file_count_lower_bound").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn watched_link_inside_untracked_directory_tracks_ignore_and_index_changes() {
        use std::os::unix::fs::symlink;

        let (_temp, repo, scope) = fixture();
        std::fs::create_dir(repo.join("new")).unwrap();
        std::fs::write(repo.join("new/sibling"), "sibling").unwrap();
        symlink("target", repo.join("new/link")).unwrap();
        let watched = [PathBuf::from("new/link")];
        let untracked = status(&repo, &scope, &watched);
        assert_eq!(untracked["file_count_lower_bound"], 1);
        assert_eq!(untracked["files_truncated"], true);
        assert_eq!(untracked["files"][0]["status"], "untracked");
        std::fs::write(repo.join(".gitignore"), "new/link\n").unwrap();
        let ignored = status(&repo, &scope, &watched);
        assert!(ignored["files"].as_array().unwrap().is_empty());
        let repository = Repository::open(&repo).unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("new/link")).unwrap();
        index.write().unwrap();
        let staged = status(&repo, &scope, &watched);
        assert_eq!(staged["files"][0]["status"], "added");
        assert_eq!(staged["files"][0]["staged"], true);
    }

    #[cfg(unix)]
    #[test]
    fn watched_symlinks_use_literal_names_and_isolate_outside_paths() {
        use std::os::unix::fs::symlink;

        let (temp, repo, scope) = fixture();
        std::fs::create_dir(repo.join("new")).unwrap();
        symlink("target", repo.join("new/link*")).unwrap();
        symlink("target", repo.join("new/linkX")).unwrap();
        symlink("target", repo.join("new/ignored")).unwrap();
        std::fs::write(repo.join(".gitignore"), "new/ignored\n").unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, repo.join("outside")).unwrap();
        let watched = ["new/link*", "new/linkX", "new/ignored", "outside/link"].map(PathBuf::from);
        let result = status(&repo, &scope, &watched);
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "new/link*");
        assert_eq!(files[0]["status"], "untracked");
        assert_eq!(files[1]["path"], "new/linkX");
        assert_eq!(files[1]["status"], "untracked");
        assert_eq!(result["watched_errors"].as_array().unwrap().len(), 1);
        assert_eq!(result["watched_errors"][0]["path"], "outside/link");
        assert_eq!(result["watched_errors"][0]["code"], "outside_workspace");
    }

    #[cfg(unix)]
    #[test]
    fn watched_symlink_case_alias_uses_canonical_status_file_behavior() {
        use crate::git::status::watched_file;
        use std::os::unix::fs::symlink;

        let (_temp, repo_path, scope) = fixture();
        std::fs::create_dir(repo_path.join("new")).unwrap();
        symlink("target", repo_path.join("new/Link")).unwrap();
        let repo = Repository::open(&repo_path).unwrap();
        repo.config()
            .unwrap()
            .set_bool("core.ignorecase", true)
            .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("new/Link")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "track link",
            &tree,
            &[],
        )
        .unwrap();
        std::fs::remove_file(repo_path.join("new/Link")).unwrap();
        symlink("changed", repo_path.join("new/Link")).unwrap();
        let alias = Path::new("new/link");
        assert!(
            watched_file(&repo, Path::new("new/Link"))
                .unwrap()
                .is_some()
        );
        if let Err(error) = repo_path.join(alias).symlink_metadata() {
            if error.kind() == std::io::ErrorKind::NotFound {
                eprintln!("skipping case-alias repository fixture: filesystem is case-sensitive");
                return;
            }
            panic!("reading case-alias fixture failed: {error}");
        }
        assert!(watched_file(&repo, alias).unwrap().is_some());
        let response = status(&repo_path, &scope, &[alias.to_path_buf()]);
        assert_eq!(
            response["files"],
            serde_json::json!([{
                "path": "new/link", "status": "modified", "staged": false,
                "unstaged": true, "is_submodule": false
            }])
        );
        assert!(response["watched_errors"].as_array().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn watched_path_outside_scope_reports_an_error_without_losing_other_status() {
        use std::os::unix::fs::symlink;

        let (temp, repo, scope) = fixture();
        let dir = repo.join("dir");
        std::fs::create_dir(&dir).unwrap();
        symlink("target", dir.join("link")).unwrap();
        std::fs::write(repo.join("other"), "other").unwrap();
        let watched = [PathBuf::from("dir/link"), PathBuf::from("other")];
        assert_eq!(
            status(&repo, &scope, &watched)["files"][0]["status"],
            "untracked"
        );
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::rename(&dir, temp.path().join("saved-dir")).unwrap();
        symlink(&outside, &dir).unwrap();
        let result = status(&repo, &scope, &watched);
        assert!(result["file_count_lower_bound"].as_u64().unwrap() > 0);
        assert_eq!(result["files"].as_array().unwrap().len(), 1);
        assert_eq!(result["files"][0]["path"], "other");
        assert_eq!(result["watched_errors"].as_array().unwrap().len(), 1);
        assert_eq!(result["watched_errors"][0]["path"], "dir/link");
        assert_eq!(result["watched_errors"][0]["code"], "outside_workspace");
    }

    #[cfg(unix)]
    #[test]
    fn compact_status_still_reports_non_utf8_file_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (_temp, repo, scope) = fixture();
        let invalid = OsString::from_vec(vec![b'x', 0xff]);
        assert!(super::utf8_path(Path::new(&invalid)).is_err());
        if let Err(error) = std::fs::write(repo.join(&invalid), "file") {
            if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EILSEQ) {
                eprintln!(
                    "skipping non-UTF-8 repository fixture: filesystem rejects invalid bytes"
                );
                return;
            }
            panic!("creating non-UTF-8 repository fixture failed: {error}");
        }
        for compact in [false, true] {
            let error = query(
                &repo,
                &scope,
                &SubmoduleConfig::default(),
                false,
                compact,
                &[],
            )
            .unwrap_err();
            assert_eq!(error.code, "repository_error");
            assert!(error.message.contains("non-UTF-8"));
        }
    }

    #[test]
    fn invalid_or_excessive_watched_paths_are_rejected() {
        let (_temp, repo, scope) = fixture();
        for watched in [
            vec![PathBuf::from("../outside")],
            vec![repo.join("absolute")],
            vec![PathBuf::from("directory/")],
            vec![PathBuf::from("file"); 257],
        ] {
            let error = query(
                &repo,
                &scope,
                &SubmoduleConfig::default(),
                false,
                true,
                &watched,
            )
            .unwrap_err();
            assert_eq!(error.code, "invalid_request");
        }
    }

    #[cfg(unix)]
    #[test]
    fn compact_status_stays_small_when_legacy_status_exceeds_transport_limit() {
        let (_temp, repo, scope) = fixture();
        let suffix = "x".repeat(190);
        for index in 0..60_000 {
            std::fs::File::create(repo.join(format!("{index:06}-{suffix}"))).unwrap();
        }
        let legacy = query(
            &repo,
            &scope,
            &SubmoduleConfig::default(),
            false,
            false,
            &[],
        )
        .unwrap();
        assert!(serde_json::to_vec(&legacy).unwrap().len() > 16 * 1024 * 1024);
        let watched = [PathBuf::from(format!("{:06}-{suffix}", 59_999))];
        let compact = status(&repo, &scope, &watched);
        assert_eq!(compact["file_count_lower_bound"], 60_000);
        assert_eq!(compact["files_truncated"], true);
        assert_eq!(compact["files"].as_array().unwrap().len(), 1);
        assert!(serde_json::to_vec(&compact).unwrap().len() < 4096);
    }
}
