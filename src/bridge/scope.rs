use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::{BridgeError, changes::DirectoryIdentity};

#[cfg(test)]
mod tests;

struct Root {
    path: PathBuf,
    source: PathBuf,
    identity: DirectoryIdentity,
}

impl Root {
    fn is_current(&self) -> bool {
        self.source
            .canonicalize()
            .is_ok_and(|path| path == self.path)
            && DirectoryIdentity::read(&self.path).is_ok_and(|identity| identity == self.identity)
    }
}

#[derive(Default)]
pub(super) struct Scope {
    roots: Vec<Root>,
}

impl Scope {
    pub(super) fn validate_worktree_removal(&self, target: &Path) -> Result<(), BridgeError> {
        for root in &self.roots {
            if !root.is_current() {
                return Err(BridgeError::new(
                    "stale_target",
                    "Workspace folder changed; refresh before continuing.",
                ));
            }
            let source_path = if let (Some(parent), Some(name)) =
                (root.source.parent(), root.source.file_name())
            {
                parent
                    .canonicalize()
                    .map_err(|error| {
                        BridgeError::new(
                            "stale_target",
                            format!("Workspace folder changed: {error}"),
                        )
                    })?
                    .join(name)
            } else {
                root.path.clone()
            };
            if (root.path != target && root.path.starts_with(target))
                || (source_path != target && source_path.starts_with(target))
            {
                return Err(BridgeError::new(
                    "invalid_request",
                    "Cannot remove a worktree containing another workspace folder.",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn new(roots: Vec<PathBuf>) -> Result<Self, BridgeError> {
        Self::new_with_identities(roots, None, None)
    }

    pub(super) fn new_with_identities(
        roots: Vec<PathBuf>,
        expected: Option<Vec<String>>,
        sources: Option<Vec<PathBuf>>,
    ) -> Result<Self, BridgeError> {
        if expected
            .as_ref()
            .is_some_and(|values| values.len() != roots.len())
        {
            return Err(BridgeError::new(
                "invalid_request",
                "workspace root identity count does not match roots",
            ));
        }
        if sources
            .as_ref()
            .is_some_and(|values| values.len() != roots.len())
        {
            return Err(BridgeError::new(
                "invalid_request",
                "workspace root source count does not match roots",
            ));
        }
        let sources = sources.unwrap_or_else(|| roots.clone());
        let mut validated = Vec::with_capacity(roots.len());
        for (index, (root, source)) in roots.into_iter().zip(sources).enumerate() {
            if !root.is_absolute() {
                return Err(BridgeError::new(
                    "invalid_request",
                    "workspace roots must be absolute paths",
                ));
            }
            let canonical = root.canonicalize().map_err(|error| {
                BridgeError::new(
                    if expected.is_some()
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        )
                    {
                        "stale_target"
                    } else {
                        "invalid_request"
                    },
                    format!("invalid workspace root: {error}"),
                )
            })?;
            if !canonical.is_dir() {
                return Err(BridgeError::new(
                    if expected.is_some() {
                        "stale_target"
                    } else {
                        "invalid_request"
                    },
                    "workspace roots must be directories",
                ));
            }
            if !source.is_absolute() {
                return Err(BridgeError::new(
                    "invalid_request",
                    "workspace root sources must be absolute paths",
                ));
            }
            let current_source = source.canonicalize().map_err(|error| {
                BridgeError::new(
                    "stale_target",
                    format!("Workspace folder is unavailable: {error}"),
                )
            })?;
            if current_source != canonical {
                return Err(BridgeError::new(
                    "stale_target",
                    "Workspace folder changed; refresh before continuing.",
                ));
            }
            let identity = DirectoryIdentity::read(&canonical).map_err(|error| {
                BridgeError::new(
                    "invalid_request",
                    format!("invalid workspace root: {}", error.message),
                )
            })?;
            if let Some(expected) = &expected {
                let token = &expected[index];
                if token.len() != 64
                    || !token
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(BridgeError::new(
                        "invalid_request",
                        "invalid workspace root identity",
                    ));
                }
                if identity.fingerprint()? != *token {
                    return Err(BridgeError::new(
                        "stale_target",
                        "Workspace root changed; refresh before continuing.",
                    ));
                }
            }
            validated.push(Root {
                path: canonical,
                source,
                identity,
            });
        }
        Ok(Self { roots: validated })
    }

    pub(super) fn identities(&self) -> Result<serde_json::Value, BridgeError> {
        let mut roots = Vec::with_capacity(self.roots.len());
        for root in &self.roots {
            if !root.is_current() {
                return Err(BridgeError::new(
                    "stale_target",
                    "Workspace folder changed; refresh before continuing.",
                ));
            }
            let current = DirectoryIdentity::read(&root.path).map_err(|error| {
                BridgeError::new(
                    "stale_target",
                    format!("Workspace root is unavailable: {}", error.message),
                )
            })?;
            if current != root.identity {
                return Err(BridgeError::new(
                    "stale_target",
                    "Workspace root changed; refresh before continuing.",
                ));
            }
            let path = root.path.to_str().ok_or_else(|| {
                BridgeError::new("repository_error", "workspace root is not valid UTF-8")
            })?;
            let source = root.source.to_str().ok_or_else(|| {
                BridgeError::new("repository_error", "workspace folder is not valid UTF-8")
            })?;
            roots.push(serde_json::json!({"path": path, "source": source, "identity": root.identity.fingerprint()?}));
        }
        Ok(serde_json::json!({"roots": roots}))
    }

    pub(super) fn allows(&self, path: &Path) -> bool {
        path.canonicalize().is_ok_and(|canonical| {
            self.roots
                .iter()
                .any(|root| canonical.starts_with(&root.path) && root.is_current())
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

    pub(super) fn validate_modules_file(&self, parent: &Path) -> Result<(), BridgeError> {
        match parent.join(".gitmodules").symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(BridgeError::new(
                "outside_workspace",
                ".gitmodules must not be a symlink",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BridgeError::new("repository_error", error.to_string())),
        }
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
            self.validate_modules_file(&parent)?;
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
                if let Some(blocker) = crate::git::status::regular_submodule_blocker(&child)
                    .map_err(|error| BridgeError::new("repository_error", error.to_string()))?
                {
                    if !self.allows(&blocker.path) {
                        return Err(BridgeError::new(
                            "outside_workspace",
                            "submodule working tree is outside the current workspace",
                        ));
                    }
                    if crate::git::status::regular_submodule_blocker(&child)
                        .map_err(|error| BridgeError::new("stale_target", error.to_string()))?
                        .as_ref()
                        != Some(&blocker)
                        || !self.allows(&blocker.path)
                    {
                        return Err(BridgeError::new(
                            "stale_target",
                            "Submodule path changed while validating the workspace.",
                        ));
                    }
                    continue;
                }
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
