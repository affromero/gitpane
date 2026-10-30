#[cfg(any(not(unix), test))]
use std::fs;
#[cfg(not(unix))]
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::git::file_ops::{self, FileOperation};

use super::BridgeError;

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn update(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

#[cfg(not(unix))]
fn metadata_identity(metadata: &Metadata) -> io::Result<Vec<u8>> {
    let mut identity = Vec::new();
    identity.extend_from_slice(&metadata.len().to_be_bytes());
    identity.push(u8::from(metadata.permissions().readonly()));
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    identity.extend_from_slice(&modified.as_secs().to_be_bytes());
    identity.extend_from_slice(&modified.subsec_nanos().to_be_bytes());
    Ok(identity)
}

#[cfg(not(unix))]
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the leaf itself, not a symlink or other reparse point.
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn changed() -> BridgeError {
    BridgeError::new(
        "file_changed",
        "file changed while its identity was being read",
    )
}

fn too_large() -> BridgeError {
    BridgeError::new(
        "file_too_large",
        "file exceeds the 64 MiB confirmation limit",
    )
}

fn text_too_large() -> BridgeError {
    BridgeError::new(
        "invalid_request",
        "Pointer side exceeds the 1 MiB text limit.",
    )
}

fn hash_file(
    hash: &mut Sha256,
    file: &mut impl Read,
    text_limit: Option<usize>,
) -> Result<Option<Vec<u8>>, BridgeError> {
    let mut total = 0u64;
    let mut bytes = [0; 64 * 1024];
    let mut text = text_limit.map(|_| Vec::new());
    loop {
        let count = file.read(&mut bytes).map_err(error)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > MAX_FILE_BYTES {
            return Err(too_large());
        }
        if text_limit.is_some_and(|limit| total > limit as u64) {
            return Err(text_too_large());
        }
        hash.update(&bytes[..count]);
        if let Some(text) = &mut text {
            text.extend_from_slice(&bytes[..count]);
        }
    }
    Ok(text)
}

#[cfg(not(unix))]
fn working_entry(
    hash: &mut Sha256,
    path: &Path,
    link_only: bool,
    text_limit: Option<usize>,
) -> Result<Option<Vec<u8>>, BridgeError> {
    // Windows opens the leaf as a reparse point, but ancestor components can
    // still change between validation and open. Revalidation cannot make the
    // following Git subprocess atomic with respect to other processes.
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if link_only || text_limit.is_some() {
                return Err(changed());
            }
            update(hash, b"missing");
            return Ok(None);
        }
        Err(err) => return Err(error(err)),
    };
    let identity = metadata_identity(&before).map_err(error)?;
    update(hash, &identity);
    let link;
    if before.file_type().is_symlink() {
        if text_limit.is_some() {
            return Err(changed());
        }
        update(hash, b"symlink");
        let target = fs::read_link(path).map_err(error)?;
        let bytes = if link_only {
            target
                .to_str()
                .ok_or_else(|| {
                    BridgeError::new("repository_error", "symlink target is not valid UTF-8")
                })?
                .as_bytes()
                .to_vec()
        } else {
            target.as_os_str().as_encoded_bytes().to_vec()
        };
        if link_only && bytes.len() >= 1024 * 1024 {
            return Err(too_large());
        }
        update(hash, &bytes);
        link = Some(bytes);
    } else if before.is_file() {
        if link_only {
            return Err(changed());
        }
        if text_limit.is_some_and(|limit| before.len() > limit as u64) {
            return Err(text_too_large());
        }
        if before.len() > MAX_FILE_BYTES {
            return Err(too_large());
        }
        update(hash, b"file");
        let mut file = open_regular(path).map_err(error)?;
        let opened = file.metadata().map_err(error)?;
        if !opened.is_file() || metadata_identity(&opened).map_err(error)? != identity {
            return Err(changed());
        }
        link = hash_file(hash, &mut file, text_limit)?;
        if metadata_identity(&file.metadata().map_err(error)?).map_err(error)? != identity {
            return Err(changed());
        }
    } else {
        return Err(BridgeError::new(
            "invalid_request",
            "directory or special file actions cannot be confirmed",
        ));
    }
    let after = fs::symlink_metadata(path).map_err(error)?;
    if before.file_type() != after.file_type()
        || metadata_identity(&after).map_err(error)? != identity
    {
        return Err(changed());
    }
    Ok(link)
}

#[cfg(unix)]
fn working_entry(
    hash: &mut Sha256,
    path: &Path,
    link_only: bool,
    text_limit: Option<usize>,
) -> Result<Option<Vec<u8>>, BridgeError> {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes()).map_err(io::Error::other)
    }

    fn open_dir(parent: &OwnedFd, component: &OsStr) -> io::Result<OwnedFd> {
        let name = name(component)?;
        // Each opened directory stays pinned; no component is followed as a symlink.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn stat(parent: &OwnedFd, name: &CString) -> io::Result<libc::stat> {
        let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { value.assume_init() })
    }

    fn directory_identity(directory: &OwnedFd) -> io::Result<(libc::dev_t, libc::ino_t)> {
        let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(directory.as_raw_fd(), value.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let value = unsafe { value.assume_init() };
        Ok((value.st_dev, value.st_ino))
    }

    fn identity(value: &libc::stat) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&value.st_dev.to_be_bytes());
        bytes.extend_from_slice(&value.st_ino.to_be_bytes());
        bytes.extend_from_slice(&value.st_mode.to_be_bytes());
        bytes.extend_from_slice(&value.st_size.to_be_bytes());
        bytes.extend_from_slice(&value.st_mtime.to_be_bytes());
        bytes.extend_from_slice(&value.st_ctime.to_be_bytes());
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "macos",
            target_os = "ios"
        ))]
        {
            bytes.extend_from_slice(&value.st_mtime_nsec.to_be_bytes());
            bytes.extend_from_slice(&value.st_ctime_nsec.to_be_bytes());
        }
        bytes
    }

    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(BridgeError::new(
            "invalid_request",
            "file path must be absolute",
        ));
    }
    let parts: Vec<_> = components
        .map(|part| match part {
            Component::Normal(value) => Ok(value),
            _ => Err(BridgeError::new(
                "invalid_request",
                "invalid file path component",
            )),
        })
        .collect::<Result<_, _>>()?;
    let (leaf, parents) = parts
        .split_last()
        .ok_or_else(|| BridgeError::new("invalid_request", "file path has no leaf"))?;
    let root = std::fs::File::open("/").map_err(error)?;
    let mut directory: OwnedFd = root.into();
    let mut parents_identity = Vec::new();
    for part in parents {
        directory = match open_dir(&directory, part) {
            Ok(fd) => fd,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if link_only || text_limit.is_some() {
                    return Err(changed());
                }
                update(hash, b"missing");
                return Ok(None);
            }
            Err(err) => return Err(error(err)),
        };
        parents_identity.push(directory_identity(&directory).map_err(error)?);
    }
    let leaf = name(leaf).map_err(error)?;
    let before = match stat(&directory, &leaf) {
        Ok(value) => value,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if link_only || text_limit.is_some() {
                return Err(changed());
            }
            update(hash, b"missing");
            return Ok(None);
        }
        Err(err) => return Err(error(err)),
    };
    let before_identity = identity(&before);
    update(hash, &before_identity);
    let kind = before.st_mode & libc::S_IFMT;
    let link;
    if kind == libc::S_IFLNK {
        if text_limit.is_some() {
            return Err(changed());
        }
        update(hash, b"symlink");
        let mut target = vec![0u8; 1024 * 1024];
        let count = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                leaf.as_ptr(),
                target.as_mut_ptr().cast(),
                target.len(),
            )
        };
        if count < 0 {
            return Err(error(io::Error::last_os_error()));
        }
        if count as usize == target.len() {
            return Err(too_large());
        }
        update(hash, &target[..count as usize]);
        target.truncate(count as usize);
        link = Some(target);
    } else if kind == libc::S_IFREG {
        if link_only {
            return Err(changed());
        }
        if before.st_size >= 0
            && text_limit.is_some_and(|limit| before.st_size as u64 > limit as u64)
        {
            return Err(text_too_large());
        }
        if before.st_size < 0 || before.st_size as u64 > MAX_FILE_BYTES {
            return Err(too_large());
        }
        update(hash, b"file");
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error(io::Error::last_os_error()));
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut opened = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(file.as_raw_fd(), opened.as_mut_ptr()) } != 0 {
            return Err(error(io::Error::last_os_error()));
        }
        let opened = unsafe { opened.assume_init() };
        if identity(&opened) != before_identity {
            return Err(changed());
        }
        link = hash_file(hash, &mut file, text_limit)?;
        let mut after_read = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(file.as_raw_fd(), after_read.as_mut_ptr()) } != 0 {
            return Err(error(io::Error::last_os_error()));
        }
        if identity(&unsafe { after_read.assume_init() }) != before_identity {
            return Err(changed());
        }
    } else {
        return Err(BridgeError::new(
            "invalid_request",
            "directory or special file actions cannot be confirmed",
        ));
    }
    if identity(&stat(&directory, &leaf).map_err(error)?) != before_identity {
        return Err(changed());
    }
    let root = std::fs::File::open("/").map_err(error)?;
    let mut current: OwnedFd = root.into();
    for (part, expected) in parents.iter().zip(parents_identity) {
        current = open_dir(&current, part).map_err(error)?;
        if directory_identity(&current).map_err(error)? != expected {
            return Err(changed());
        }
    }
    if identity(&stat(&current, &leaf).map_err(error)?) != before_identity {
        return Err(changed());
    }
    Ok(link)
}

/// Read a symlink entry without following it or any directory component.
pub(super) fn link_target(path: &Path) -> Result<String, BridgeError> {
    let bytes = working_entry(&mut Sha256::new(), path, true, None)?.ok_or_else(changed)?;
    String::from_utf8(bytes)
        .map_err(|_| BridgeError::new("repository_error", "symlink target is not valid UTF-8"))
}

/// Read an unchanged regular file without following symlinks in its path.
#[cfg(unix)]
pub(super) fn working_text(path: &Path, limit: usize) -> Result<(String, String), BridgeError> {
    let mut hash = Sha256::new();
    let bytes = working_entry(&mut hash, path, false, Some(limit))?.ok_or_else(changed)?;
    if bytes.contains(&0) {
        return Err(BridgeError::new(
            "invalid_request",
            "Binary pointer sides cannot be opened as text.",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| BridgeError::new("invalid_request", "Pointer side is not UTF-8 text."))?;
    Ok((text, format!("{:x}", hash.finalize())))
}

#[cfg(not(unix))]
pub(super) fn working_text(_path: &Path, _limit: usize) -> Result<(String, String), BridgeError> {
    Err(BridgeError::new(
        "unsupported",
        "Working submodule type comparisons are unavailable on this platform.",
    ))
}

/// Build a token for the exact canonical file-operation plan and its inputs.
/// `validate_path` must reject paths outside the repository and workspace.
pub(super) fn token(
    repo_path: &Path,
    selected: &Path,
    operation: FileOperation,
    validate_path: impl Fn(&Path) -> Result<(), BridgeError>,
) -> Result<String, BridgeError> {
    Ok(plan(repo_path, selected, operation, validate_path)?.0)
}

/// Return the identity and the exact Git arguments covered by that identity.
pub(super) fn plan(
    repo_path: &Path,
    selected: &Path,
    operation: FileOperation,
    validate_path: impl Fn(&Path) -> Result<(), BridgeError>,
) -> Result<(String, Vec<String>), BridgeError> {
    let operation_name = match operation {
        FileOperation::Discard => "discard",
        FileOperation::DeleteUntracked => "delete_untracked",
        _ => {
            return Err(BridgeError::new(
                "invalid_request",
                "identity requires a destructive file operation",
            ));
        }
    };
    validate_path(selected)?;
    let args = file_ops::arguments(repo_path, selected, operation).map_err(error)?;
    let paths: Vec<PathBuf> = args
        .iter()
        .skip_while(|arg| *arg != "--")
        .skip(1)
        .map(PathBuf::from)
        .collect();
    if paths.is_empty() {
        return Err(BridgeError::new(
            "invalid_request",
            "file action has no target",
        ));
    }
    for path in &paths {
        validate_path(path)?;
    }
    let repo = git2::Repository::open(repo_path).map_err(error)?;
    let mut hash = Sha256::new();
    update(&mut hash, b"gitpane-file-identity-v1");
    update(&mut hash, repo_path.as_os_str().as_encoded_bytes());
    update(&mut hash, operation_name.as_bytes());
    let head_ref = repo.find_reference("HEAD").map_err(error)?;
    update(
        &mut hash,
        head_ref
            .symbolic_target()
            .map_err(error)?
            .unwrap_or("detached")
            .as_bytes(),
    );
    match repo.head() {
        Ok(head) => {
            let oid = head.target().ok_or_else(|| error("HEAD has no commit"))?;
            update(&mut hash, oid.as_bytes());
        }
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => update(&mut hash, b"unborn"),
        Err(err) => return Err(error(err)),
    }
    let status = repo.status_file(selected).map_err(error)?;
    update(&mut hash, &status.bits().to_be_bytes());
    let index = repo.index().map_err(error)?;
    for path in paths {
        update(&mut hash, path.as_os_str().as_encoded_bytes());
        for stage in 0..=3 {
            match index.get_path(&path, stage) {
                Some(entry) => {
                    update(&mut hash, b"index");
                    update(&mut hash, &entry.mode.to_be_bytes());
                    update(&mut hash, entry.id.as_bytes());
                    update(&mut hash, &entry.flags.to_be_bytes());
                    update(&mut hash, &entry.flags_extended.to_be_bytes());
                }
                None => update(&mut hash, b"no-index-entry"),
            }
        }
        working_entry(&mut hash, &repo_path.join(path), false, None)?;
    }
    Ok((format!("{:x}", hash.finalize()), args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};

    fn fixture() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("file"), b"original").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    #[cfg(unix)]
    #[test]
    fn working_text_reads_regular_files_and_refuses_symlink_paths() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("inside")).unwrap();
        fs::write(root.join("inside/file"), b"first\n").unwrap();
        let (first, identity) = working_text(&root.join("inside/file"), 1024).unwrap();
        assert_eq!(first, "first\n");
        fs::write(root.join("inside/file"), b"other\n").unwrap();
        assert_ne!(
            working_text(&root.join("inside/file"), 1024).unwrap().1,
            identity
        );
        symlink(root.join("inside/file"), root.join("leaf")).unwrap();
        assert!(working_text(&root.join("leaf"), 1024).is_err());
        symlink(root.join("inside"), root.join("ancestor")).unwrap();
        assert!(working_text(&root.join("ancestor/file"), 1024).is_err());
    }

    fn identity(repo: &Path, path: &str, operation: FileOperation) -> String {
        token(
            &repo.canonicalize().unwrap(),
            Path::new(path),
            operation,
            |_| Ok(()),
        )
        .unwrap()
    }

    #[test]
    fn worktree_and_index_changes_invalidate_identity_even_when_status_stays_modified() {
        let (dir, repo) = fixture();
        let path = dir.path().join("file");
        fs::write(&path, b"changed1").unwrap();
        let first = identity(dir.path(), "file", FileOperation::Discard);
        assert_eq!(first, identity(dir.path(), "file", FileOperation::Discard));
        fs::write(&path, b"changed2").unwrap();
        let second = identity(dir.path(), "file", FileOperation::Discard);
        assert_ne!(first, second);
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        fs::write(&path, b"changed3").unwrap();
        let staged = identity(dir.path(), "file", FileOperation::Discard);
        let mut entry = index.get_path(Path::new("file"), 0).unwrap();
        entry.id = repo.blob(b"different staged content").unwrap();
        index.add(&entry).unwrap();
        index.write().unwrap();
        assert_ne!(staged, identity(dir.path(), "file", FileOperation::Discard));
    }

    #[test]
    fn changing_head_invalidates_identity() {
        let (dir, repo) = fixture();
        fs::write(dir.path().join("file"), b"dirty").unwrap();
        let before = identity(dir.path(), "file", FileOperation::Discard);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = head.tree().unwrap();
        let signature = Signature::now("Test", "test@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "another",
            &tree,
            &[&head],
        )
        .unwrap();
        assert_ne!(before, identity(dir.path(), "file", FileOperation::Discard));
    }

    #[test]
    fn untracked_file_changes_invalidate_identity_and_directories_are_refused() {
        let (dir, _) = fixture();
        fs::write(dir.path().join("untracked"), b"first").unwrap();
        let before = identity(dir.path(), "untracked", FileOperation::DeleteUntracked);
        fs::write(dir.path().join("untracked"), b"other").unwrap();
        assert_ne!(
            before,
            identity(dir.path(), "untracked", FileOperation::DeleteUntracked)
        );
        fs::create_dir(dir.path().join("directory")).unwrap();
        assert!(
            token(
                dir.path(),
                Path::new("directory"),
                FileOperation::DeleteUntracked,
                |_| Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_files_are_refused_before_confirmation() {
        let (dir, _) = fixture();
        let file = fs::File::create(dir.path().join("large")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let result = token(
            &dir.path().canonicalize().unwrap(),
            Path::new("large"),
            FileOperation::DeleteUntracked,
            |_| Ok(()),
        );
        assert_eq!(result.unwrap_err().code, "file_too_large");
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_is_refused_during_identity_read() {
        use std::os::unix::fs::symlink;
        let (dir, _) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "outside").unwrap();
        symlink(outside.path(), dir.path().join("linkdir")).unwrap();
        let mut hash = Sha256::new();
        let path = dir.path().canonicalize().unwrap().join("linkdir/secret");
        assert!(working_entry(&mut hash, &path, false, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlinks_are_identified_by_target_without_following_it() {
        use std::os::unix::fs::symlink;
        let (dir, _) = fixture();
        let link = dir.path().join("link");
        symlink("missing-one", &link).unwrap();
        let before = identity(dir.path(), "link", FileOperation::DeleteUntracked);
        fs::remove_file(&link).unwrap();
        symlink("missing-two", &link).unwrap();
        assert_ne!(
            before,
            identity(dir.path(), "link", FileOperation::DeleteUntracked)
        );
    }

    #[test]
    fn recreated_rename_source_prevents_discard_confirmation() {
        if !crate::git::git_test_available() {
            return;
        }
        let (dir, _) = fixture();
        let result = crate::git::process::git_command(dir.path())
            .args(["mv", "file", "renamed"])
            .output()
            .unwrap();
        assert!(result.status.success());
        let before = identity(dir.path(), "renamed", FileOperation::Discard);
        assert!(!before.is_empty());
        fs::write(dir.path().join("file"), b"replacement").unwrap();
        assert!(
            token(
                dir.path(),
                Path::new("renamed"),
                FileOperation::Discard,
                |_| Ok(())
            )
            .is_err()
        );
    }
}
