use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::SubmoduleConfig;
use crate::git::status::{ChangeError, collect_changes};

use super::{BridgeError, scope::Scope};

#[cfg(test)]
mod tests;

const MAX_RECORDS: usize = 200_000;
const MAX_SNAPSHOTS: usize = 64;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 256 * 1024;
const MAX_PAGE_ITEMS: usize = 128;
const EXPIRY: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Begin {
    repo: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageRequest {
    repo: PathBuf,
    snapshot: String,
    cursor: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FindRequest {
    repo: PathBuf,
    snapshot: String,
    path: String,
    group: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Release {
    repo: PathBuf,
    snapshot: String,
}

#[derive(PartialEq, Eq, Serialize)]
pub(super) struct DirectoryIdentity {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    created: Option<std::time::SystemTime>,
}

impl DirectoryIdentity {
    pub(super) fn fingerprint(&self) -> Result<String, BridgeError> {
        let bytes = serde_json::to_vec(self).map_err(repository_error)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub(super) fn read(path: &Path) -> Result<Self, BridgeError> {
        let path = path.canonicalize().map_err(repository_error)?;
        let metadata = fs::metadata(&path).map_err(repository_error)?;
        if !metadata.is_dir() {
            return Err(BridgeError::new(
                "repository_error",
                "repository directory is unavailable",
            ));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        let (volume, file_index) = windows_directory_identity(&path)?;
        Ok(Self {
            path,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(windows)]
            volume,
            #[cfg(windows)]
            file_index,
            #[cfg(not(any(unix, windows)))]
            created: metadata.created().ok(),
        })
    }
}

#[cfg(windows)]
fn windows_directory_identity(path: &Path) -> Result<(u32, u64), BridgeError> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES, GetFileInformationByHandle,
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(repository_error)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(repository_error(std::io::Error::last_os_error()));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(repository_error("repository directory is unavailable"));
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[derive(PartialEq, Eq, Serialize)]
pub(super) struct RepositoryIdentity {
    worktree: DirectoryIdentity,
    gitdir: DirectoryIdentity,
    common: DirectoryIdentity,
}

impl RepositoryIdentity {
    pub(super) fn same_common(&self, other: &Self) -> bool {
        self.common == other.common
    }

    pub(super) fn fingerprint(&self) -> Result<String, BridgeError> {
        let bytes = serde_json::to_vec(self).map_err(repository_error)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub(super) fn read(repo: &Path) -> Result<Self, BridgeError> {
        let repository = git2::Repository::open(repo).map_err(repository_error)?;
        Ok(Self {
            worktree: DirectoryIdentity::read(repo)?,
            gitdir: DirectoryIdentity::read(repository.path())?,
            common: DirectoryIdentity::read(repository.commondir())?,
        })
    }
}

struct Snapshot {
    repo: PathBuf,
    identity: RepositoryIdentity,
    revision: String,
    records: Vec<Box<[u8]>>,
    page_starts: Vec<usize>,
    bytes: usize,
    touched: Instant,
}

#[derive(Default)]
pub(super) struct Snapshots {
    entries: HashMap<String, Snapshot>,
    sequence: u64,
}

fn repository_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", error.to_string())
}

fn change_error(error: ChangeError) -> BridgeError {
    match error {
        ChangeError::Git(error) => repository_error(error),
        ChangeError::Repository(error) => repository_error(error),
        ChangeError::InvalidPath => BridgeError::new(
            "repository_error",
            "A changed path is invalid or is not UTF-8.",
        ),
        ChangeError::Limit => BridgeError::new(
            "snapshot_too_large",
            "The repository has too many changes to list.",
        ),
    }
}

pub(super) fn current_record(
    repo: &git2::Repository,
    sub_cfg: &SubmoduleConfig,
    path: &str,
    group: &str,
) -> Result<Option<Value>, BridgeError> {
    let mut bytes = 0usize;
    let mut found = None;
    for record in collect_changes(repo, sub_cfg, MAX_RECORDS).map_err(change_error)? {
        let encoded = serde_json::to_vec(&record).map_err(repository_error)?;
        account_record(&mut bytes, encoded.len())?;
        if record.matches(path, group) {
            found = Some(serde_json::from_slice(&encoded).map_err(repository_error)?);
        }
    }
    Ok(found)
}

fn account_record(bytes: &mut usize, size: usize) -> Result<(), BridgeError> {
    if size.saturating_add(1) > MAX_PAGE_BYTES {
        return Err(BridgeError::new(
            "snapshot_too_large",
            "A change is too large to include in a page.",
        ));
    }
    *bytes = bytes.saturating_add(size.saturating_add(128));
    if *bytes > MAX_CACHE_BYTES {
        return Err(BridgeError::new(
            "snapshot_too_large",
            "The repository change snapshot exceeds the memory limit.",
        ));
    }
    Ok(())
}

fn stale() -> BridgeError {
    BridgeError::new(
        "snapshot_stale",
        "Change snapshot is no longer available; refresh the repository.",
    )
}

fn page_boundaries(sizes: &[usize]) -> Result<Vec<usize>, BridgeError> {
    let mut starts = vec![0];
    let mut bytes = 0usize;
    let mut items = 0usize;
    for (index, size) in sizes.iter().copied().enumerate() {
        if size.saturating_add(1) > MAX_PAGE_BYTES {
            return Err(BridgeError::new(
                "snapshot_too_large",
                "A change is too large to include in a page.",
            ));
        }
        if items > 0 && (items >= MAX_PAGE_ITEMS || bytes + size + 1 > MAX_PAGE_BYTES) {
            starts.push(index);
            bytes = 0;
            items = 0;
        }
        bytes += size + 1;
        items += 1;
    }
    Ok(starts)
}

impl Snapshots {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    fn expire(&mut self) {
        let now = Instant::now();
        self.entries
            .retain(|_, snapshot| now.duration_since(snapshot.touched) < EXPIRY);
    }

    pub(super) fn begin(
        &mut self,
        scope: &Scope,
        input: Begin,
        sub_cfg: &SubmoduleConfig,
    ) -> Result<Value, BridgeError> {
        self.expire();
        let repo = scope.repository(&input.repo)?;
        scope.validate_submodules(&repo)?;
        let identity = RepositoryIdentity::read(&repo)?;
        let repository = git2::Repository::open(&repo).map_err(repository_error)?;
        let changes = collect_changes(&repository, sub_cfg, MAX_RECORDS).map_err(change_error)?;
        scope.repository(&repo)?;
        scope.validate_submodules(&repo)?;
        if RepositoryIdentity::read(&repo)? != identity {
            return Err(stale());
        }

        let mut records = Vec::with_capacity(changes.len());
        let mut sizes = Vec::with_capacity(changes.len());
        let mut bytes = 0usize;
        let mut hash = Sha256::new();
        for change in changes {
            let encoded = serde_json::to_vec(&change).map_err(repository_error)?;
            hash.update((encoded.len() as u64).to_be_bytes());
            hash.update(&encoded);
            // Keep serialized records rather than JSON trees so the cache has
            // a predictable per-record bound, including allocation overhead.
            account_record(&mut bytes, encoded.len())?;
            sizes.push(encoded.len());
            records.push(encoded.into_boxed_slice());
        }
        let starts = page_boundaries(&sizes)?;
        let revision = format!("{:x}", hash.finalize());
        let existing = self
            .entries
            .values()
            .filter(|entry| entry.repo != repo)
            .fold(0usize, |total, entry| total.saturating_add(entry.bytes));
        let count = self
            .entries
            .values()
            .filter(|entry| entry.repo != repo)
            .count();
        if count >= MAX_SNAPSHOTS || existing.saturating_add(bytes) > MAX_CACHE_BYTES {
            return Err(BridgeError::new(
                "snapshot_capacity",
                "Too many change snapshots are open; close one and retry.",
            ));
        }
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            BridgeError::new(
                "snapshot_capacity",
                "Change snapshot identifiers are exhausted.",
            )
        })?;
        let id = format!("changes-{}", self.sequence);
        self.entries.retain(|_, entry| entry.repo != repo);
        self.entries.insert(
            id.clone(),
            Snapshot {
                repo,
                identity,
                revision,
                records,
                page_starts: starts,
                bytes,
                touched: Instant::now(),
            },
        );
        self.page_by_id(scope, &input.repo, &id, 0)
    }

    pub(super) fn page(&mut self, scope: &Scope, input: PageRequest) -> Result<Value, BridgeError> {
        self.expire();
        self.page_by_id(scope, &input.repo, &input.snapshot, input.cursor)
    }

    pub(super) fn find(&mut self, scope: &Scope, input: FindRequest) -> Result<Value, BridgeError> {
        self.expire();
        let path = Path::new(&input.path);
        if path.as_os_str().is_empty()
            || input.path.contains('\0')
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || !matches!(
                input.group.as_str(),
                "index" | "working" | "untracked" | "merge" | "attention"
            )
        {
            return Err(BridgeError::new(
                "invalid_request",
                "invalid change path or group",
            ));
        }
        let repo = scope.repository(&input.repo)?;
        scope.validate_submodules(&repo)?;
        let snapshot = self.entries.get_mut(&input.snapshot).ok_or_else(stale)?;
        if repo != snapshot.repo || RepositoryIdentity::read(&repo)? != snapshot.identity {
            return Err(stale());
        }

        // The canonical collector sorts by path and group. Search the snapshot
        // without materializing every page or retaining another index.
        let mut lower = 0;
        let mut upper = snapshot.records.len();
        let mut found = None;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let record: Value =
                serde_json::from_slice(&snapshot.records[middle]).map_err(repository_error)?;
            let record_path = record["path"].as_str().ok_or_else(stale)?;
            let record_group = record["group"].as_str().ok_or_else(stale)?;
            match (record_path, record_group).cmp(&(input.path.as_str(), input.group.as_str())) {
                std::cmp::Ordering::Less => lower = middle + 1,
                std::cmp::Ordering::Greater => upper = middle,
                std::cmp::Ordering::Equal => {
                    found = Some(record);
                    break;
                }
            }
        }
        snapshot.touched = Instant::now();
        Ok(json!({ "change": found }))
    }

    fn page_by_id(
        &mut self,
        scope: &Scope,
        path: &Path,
        id: &str,
        cursor: usize,
    ) -> Result<Value, BridgeError> {
        let repo = scope.repository(path)?;
        let snapshot = self.entries.get_mut(id).ok_or_else(stale)?;
        if repo != snapshot.repo || RepositoryIdentity::read(&repo)? != snapshot.identity {
            return Err(stale());
        }
        let index = snapshot
            .page_starts
            .iter()
            .position(|start| *start == cursor)
            .ok_or_else(stale)?;
        let end = snapshot
            .page_starts
            .get(index + 1)
            .copied()
            .unwrap_or(snapshot.records.len());
        let changes: Vec<Value> = snapshot.records[cursor..end]
            .iter()
            .map(|record| serde_json::from_slice(record).map_err(repository_error))
            .collect::<Result<_, _>>()?;
        snapshot.touched = Instant::now();
        Ok(json!({
            "snapshot": id, "revision": snapshot.revision, "cursor": cursor,
            "previous_cursor": index.checked_sub(1).map(|i| snapshot.page_starts[i]),
            "next_cursor": snapshot.page_starts.get(index + 1),
            "changes": changes,
        }))
    }

    pub(super) fn release(&mut self, scope: &Scope, input: Release) -> Result<Value, BridgeError> {
        self.expire();
        let repo = scope.repository(&input.repo)?;
        let snapshot = self.entries.get(&input.snapshot).ok_or_else(stale)?;
        if snapshot.repo != repo || RepositoryIdentity::read(&repo)? != snapshot.identity {
            return Err(stale());
        }
        self.entries.remove(&input.snapshot);
        Ok(json!({}))
    }
}
