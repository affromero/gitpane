use std::path::PathBuf;

/// Use the ordinary Windows spelling at process and watcher boundaries while
/// keeping canonical repository identities unchanged. Other platforms borrow
/// the original path.
pub(crate) fn boundary_path(path: &std::path::Path) -> std::borrow::Cow<'_, std::path::Path> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut result = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
                Prefix::VerbatimUNC(server, share) => {
                    let mut value = std::ffi::OsString::from(r"\\");
                    value.push(server);
                    value.push(r"\");
                    value.push(share);
                    PathBuf::from(value)
                }
                _ => return std::borrow::Cow::Borrowed(path),
            };
            result.extend(components);
            return std::borrow::Cow::Owned(result);
        }
    }
    std::borrow::Cow::Borrowed(path)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn process_paths_accept_canonical_drive_and_unc_spellings() {
        for (canonical, ordinary) in [
            (r"\\?\C:\repo\source file.rs", "C:/repo/source file.rs"),
            (
                r"\\?\UNC\server\share\repo\file",
                r"\\server\share\repo\file",
            ),
        ] {
            assert_eq!(
                boundary_path(Path::new(canonical)).as_ref(),
                Path::new(ordinary)
            );
        }
        for path in [
            r"relative\file",
            r"C:\repo\file",
            r"\\.\device",
            r"\\?\Volume{abc}\file",
        ] {
            assert_eq!(boundary_path(Path::new(path)).as_ref(), Path::new(path));
        }
    }
}

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
