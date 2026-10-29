use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::BridgeError;

#[derive(Default)]
pub(super) struct Scope {
    roots: Vec<PathBuf>,
}

impl Scope {
    pub(super) fn new(roots: Vec<PathBuf>) -> Result<Self, BridgeError> {
        let mut validated = Vec::with_capacity(roots.len());
        for root in roots {
            if !root.is_absolute() {
                return Err(BridgeError::new(
                    "invalid_request",
                    "workspace roots must be absolute paths",
                ));
            }
            let canonical = root.canonicalize().map_err(|error| {
                BridgeError::new(
                    "invalid_request",
                    format!("invalid workspace root: {error}"),
                )
            })?;
            if !canonical.is_dir() {
                return Err(BridgeError::new(
                    "invalid_request",
                    "workspace roots must be directories",
                ));
            }
            if !validated.contains(&canonical) {
                validated.push(canonical);
            }
        }
        Ok(Self { roots: validated })
    }

    pub(super) fn allows(&self, path: &Path) -> bool {
        path.canonicalize().is_ok_and(|canonical| {
            self.roots.iter().any(|root| {
                root.canonicalize()
                    .is_ok_and(|current| current == *root && canonical.starts_with(&current))
            })
        })
    }

    /// Check the closest existing ancestor so uninitialized submodules can
    /// be classified without resolving a nonexistent working tree.
    pub(super) fn allows_missing(&self, path: &Path) -> Result<bool, BridgeError> {
        let mut candidate = path;
        loop {
            match candidate.canonicalize() {
                Ok(canonical) => return Ok(self.allows(&canonical)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match candidate.symlink_metadata() {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            return Err(BridgeError::new(
                                "outside_workspace",
                                "cannot validate a dangling working-tree symlink",
                            ));
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(BridgeError::new("repository_error", error.to_string()));
                        }
                    }
                    candidate = candidate.parent().ok_or_else(|| {
                        BridgeError::new(
                            "repository_error",
                            "working tree has no existing ancestor",
                        )
                    })?;
                }
                Err(error) => return Err(BridgeError::new("repository_error", error.to_string())),
            }
        }
    }

    pub(super) fn validate_submodules(&self, path: &Path) -> Result<(), BridgeError> {
        self.submodule_repositories(path).map(|_| ())
    }

    /// Validate all initialized descendants before any status query can traverse them.
    pub(super) fn submodule_repositories(&self, path: &Path) -> Result<Vec<PathBuf>, BridgeError> {
        const MAX_REPOSITORIES: usize = 1024;
        const MAX_DEPTH: usize = 32;
        let root = self.repository(path)?;
        let mut seen = HashSet::from([root.clone()]);
        let mut pending = vec![(root, 0)];
        let mut descendants = Vec::new();
        while let Some((parent, depth)) = pending.pop() {
            let modules_file = parent.join(".gitmodules");
            match modules_file.symlink_metadata() {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(BridgeError::new(
                        "outside_workspace",
                        ".gitmodules must not be a symlink",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(BridgeError::new("repository_error", error.to_string())),
            }
            let repo = git2::Repository::open(&parent)
                .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
            let submodules = repo
                .submodules()
                .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
            for submodule in submodules {
                let relative = submodule.path();
                if relative.as_os_str().is_empty()
                    || relative.components().any(|component| {
                        !matches!(component, Component::Normal(_) | Component::CurDir)
                    })
                {
                    return Err(BridgeError::new(
                        "outside_workspace",
                        "submodule path is not a relative working-tree path",
                    ));
                }
                let child = parent.join(relative);
                if !self.allows_missing(&child)? {
                    return Err(BridgeError::new(
                        "outside_workspace",
                        "submodule working tree is outside the current workspace",
                    ));
                }
                match child.join(".git").symlink_metadata() {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(BridgeError::new("repository_error", error.to_string()));
                    }
                }
                let child = self.repository(&child)?;
                if !seen.insert(child.clone()) {
                    return Err(BridgeError::new(
                        "invalid_request",
                        "submodule working trees contain a cycle or duplicate",
                    ));
                }
                if depth >= MAX_DEPTH || seen.len() > MAX_REPOSITORIES {
                    return Err(BridgeError::new(
                        "invalid_request",
                        "submodule nesting exceeds the inspection limit",
                    ));
                }
                descendants.push(child.clone());
                pending.push((child, depth + 1));
            }
        }
        Ok(descendants)
    }

    pub(super) fn repository(&self, path: &Path) -> Result<PathBuf, BridgeError> {
        if !path.is_absolute() {
            return Err(BridgeError::new(
                "invalid_request",
                "repository path must be absolute",
            ));
        }
        let canonical = path.canonicalize().map_err(|error| {
            BridgeError::new(
                "repository_error",
                format!("invalid repository path: {error}"),
            )
        })?;
        if !self.allows(&canonical) {
            return Err(BridgeError::new(
                "outside_workspace",
                "repository is outside the current workspace",
            ));
        }
        let repo = git2::Repository::open(&canonical).map_err(|error| {
            BridgeError::new(
                "repository_error",
                format!("cannot open repository: {error}"),
            )
        })?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| BridgeError::new("repository_error", "repository has no working tree"))?
            .canonicalize()
            .map_err(|error| {
                BridgeError::new("repository_error", format!("invalid working tree: {error}"))
            })?;
        if workdir != canonical || !self.allows(&workdir) {
            return Err(BridgeError::new(
                "outside_workspace",
                "path does not identify a workspace repository root",
            ));
        }
        Ok(canonical)
    }
}
