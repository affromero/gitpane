use std::path::PathBuf;

/// Stable identity for a repository, based on its filesystem path.
/// Unlike positional `usize` indices, a `RepoId` remains valid across
/// repo list mutations (add, remove, sort, rescan).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RepoId(pub PathBuf);

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}
