use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use super::super::{BridgeError, mutation, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct Request {
    repo: PathBuf,
    operation: String,
    expected_branch: Option<String>,
    expected_head: String,
}

pub(in crate::bridge) fn execute(scope: &Scope, input: Request) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    mutation::verify_sync_head(
        &repo,
        input.expected_branch.as_deref(),
        &input.expected_head,
    )?;
    if !matches!(
        input.operation.as_str(),
        "sync" | "update" | "pull_latest" | "pull_with_submodules"
    ) {
        return Err(BridgeError::new(
            "invalid_request",
            "unknown submodule action",
        ));
    }
    #[cfg(not(unix))]
    {
        Err(BridgeError::new(
            "unsupported_platform",
            "Submodule batch actions are unavailable on this platform because process-tree timeouts are not supported.",
        ))
    }
    #[cfg(unix)]
    {
        if input.operation == "pull_with_submodules" {
            unix::pull_with_submodules(scope, &repo, &input)
        } else {
            run_batch(scope, &repo, &input)
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::collections::{HashMap, HashSet};
    use std::path::{Component, Path, PathBuf};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::super::super::{BridgeError, mutation, scope::Scope};
    use crate::git::process;

    use super::Request;

    const MAX_SUBMODULES: usize = 128;
    const MAX_DEPTH: usize = 32;
    const BATCH_TIMEOUT: Duration = Duration::from_secs(300);

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Entry {
        name: String,
        path: PathBuf,
        head: Option<git2::Oid>,
        index: Option<git2::Oid>,
        url: Option<String>,
        local_url: Option<String>,
        module_update: Option<String>,
        local_update: Option<String>,
        initialized: bool,
        child_head: Option<ChildHead>,
        pull_target: Option<PullTarget>,
        skip: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ChildHead {
        branch_ref: Option<String>,
        oid: Option<git2::Oid>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct PullTarget {
        name: String,
        url: String,
        fetch: Vec<String>,
        pull_rebase: Option<String>,
        branch_rebase: Option<String>,
        pull_ff: Option<String>,
    }

    fn error(error: impl std::fmt::Display) -> BridgeError {
        BridgeError::new("repository_error", error.to_string())
    }

    fn config_value(config: &git2::Config, key: &str) -> Result<Option<String>, BridgeError> {
        match config.get_string(key) {
            Ok(value) => Ok(Some(value)),
            Err(failure) if failure.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(failure) => Err(error(failure)),
        }
    }

    fn validate_update(value: &str) -> Result<(), BridgeError> {
        if value.starts_with('!') {
            return Err(BridgeError::new(
                "invalid_request",
                "custom submodule update commands are not supported",
            ));
        }
        if !matches!(value, "checkout" | "merge" | "rebase" | "none") {
            return Err(BridgeError::new(
                "invalid_request",
                "unsupported submodule update strategy",
            ));
        }
        Ok(())
    }

    fn safe_path(scope: &Scope, repo: &Path, relative: &Path) -> Result<(), BridgeError> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(BridgeError::new(
                "invalid_request",
                "submodule path must be relative without traversal",
            ));
        }
        let target = repo.join(relative);
        if !scope.allows_missing(&target)? {
            return Err(BridgeError::new(
                "outside_workspace",
                "submodule is outside the workspace",
            ));
        }
        let mut current = repo.to_path_buf();
        for component in relative.components() {
            current.push(component.as_os_str());
            match current.symlink_metadata() {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(BridgeError::new(
                        "outside_workspace",
                        "submodule path contains a symlink",
                    ));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(BridgeError::new(
                        "invalid_request",
                        "submodule path contains a non-directory",
                    ));
                }
                Ok(_) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => break,
                Err(failure) => return Err(error(failure)),
            }
        }
        Ok(())
    }

    fn entries(scope: &Scope, path: &Path, operation: &str) -> Result<Vec<Entry>, BridgeError> {
        let modules_path = path.join(".gitmodules");
        let module_config = match modules_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(BridgeError::new(
                    "invalid_request",
                    ".gitmodules must be a regular file",
                ));
            }
            Ok(_) => Some(git2::Config::open(&modules_path).map_err(error)?),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
            Err(failure) => return Err(error(failure)),
        };
        let repo = git2::Repository::open(path).map_err(error)?;
        let config = repo.config().map_err(error)?;
        if operation == "update"
            && let Some(module_config) = &module_config
        {
            let mut config_entries = module_config.entries(None).map_err(error)?;
            while let Some(item) = config_entries.next() {
                let item = item.map_err(error)?;
                let key = item.name().map_err(error)?;
                if let Some(name) = key
                    .strip_prefix("submodule.")
                    .and_then(|name| name.strip_suffix(".path"))
                {
                    for value in [
                        config_value(&config, &format!("submodule.{name}.update"))?,
                        config_value(module_config, &format!("submodule.{name}.update"))?,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        validate_update(&value)?;
                    }
                }
            }
        }
        scope.validate_submodules(path)?;
        let subs = repo.submodules().map_err(error)?;
        if subs.len() > MAX_SUBMODULES {
            return Err(BridgeError::new(
                "invalid_request",
                "too many direct submodules for one action",
            ));
        }
        let mut result = Vec::with_capacity(subs.len());
        for sub in subs {
            let name = sub.name().map_err(error)?.to_owned();
            let relative = sub.path().to_path_buf();
            if relative.to_str().is_none() {
                return Err(BridgeError::new(
                    "repository_error",
                    "submodule path is not valid UTF-8",
                ));
            }
            safe_path(scope, path, &relative)?;
            let local_url = config_value(&config, &format!("submodule.{name}.url"))?;
            let local_update = config_value(&config, &format!("submodule.{name}.update"))?;
            let module_update = match &module_config {
                Some(config) => config_value(config, &format!("submodule.{name}.update"))?,
                None => None,
            };
            if operation == "update" {
                for value in [local_update.as_deref(), module_update.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    validate_update(value)?;
                }
            }
            let target = path.join(&relative);
            let initialized = match target.join(".git").symlink_metadata() {
                Ok(_) => {
                    scope.repository(&target)?;
                    true
                }
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => false,
                Err(failure) => return Err(error(failure)),
            };
            let child_head = if initialized && operation != "sync" {
                Some(read_head(&target)?)
            } else {
                None
            };
            let pull_target = if initialized && operation == "pull_latest" {
                Some(pull_remote(&target)?)
            } else {
                None
            };
            let index = sub.index_id();
            let skip = if operation == "sync" {
                local_url.is_none()
            } else if operation == "update" {
                index.is_none()
                    || local_update.as_deref().or(module_update.as_deref()) == Some("none")
            } else {
                !initialized
            };
            result.push(Entry {
                name,
                path: relative,
                head: sub.head_id(),
                index,
                url: sub.url().map_err(error)?.map(str::to_owned),
                local_url,
                module_update,
                local_update,
                initialized,
                child_head,
                pull_target,
                skip,
            });
        }
        result.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }

    fn read_head(path: &Path) -> Result<ChildHead, BridgeError> {
        let child = git2::Repository::open(path).map_err(error)?;
        let head = child.find_reference("HEAD").map_err(error)?;
        let branch_ref = head.symbolic_target().map_err(error)?.map(str::to_owned);
        let oid = match child.head() {
            Ok(reference) => Some(reference.peel_to_commit().map_err(error)?.id()),
            Err(failure) if failure.code() == git2::ErrorCode::UnbornBranch => None,
            Err(failure) => return Err(error(failure)),
        };
        Ok(ChildHead { branch_ref, oid })
    }

    fn pull_remote(path: &Path) -> Result<PullTarget, BridgeError> {
        let repo = git2::Repository::open(path).map_err(error)?;
        let head = read_head(path)?;
        let config = repo.config().map_err(error)?;
        let branch = head
            .branch_ref
            .as_deref()
            .and_then(|name| name.strip_prefix("refs/heads/"));
        let pull_rebase = config_value(&config, "pull.rebase")?;
        let branch_rebase = match branch {
            Some(branch) => config_value(&config, &format!("branch.{branch}.rebase"))?,
            None => None,
        };
        let pull_ff = config_value(&config, "pull.ff")?;
        let configured = if let Some(branch) = branch
            && let Some(remote) = config_value(&config, &format!("branch.{branch}.remote"))?
        {
            Some(remote)
        } else {
            None
        };
        let name = match configured {
            Some(name) => name,
            None => {
                let remotes = repo.remotes().map_err(error)?;
                let mut names = remotes
                    .iter()
                    .map(|name| {
                        name.map_err(error)?
                            .map(str::to_owned)
                            .ok_or_else(|| error("remote name is not valid UTF-8"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                names.sort();
                names
                    .into_iter()
                    .next()
                    .ok_or_else(|| error("submodule has no remote"))?
            }
        };
        if name == "." {
            return Ok(PullTarget {
                name,
                url: ".".into(),
                fetch: Vec::new(),
                pull_rebase,
                branch_rebase,
                pull_ff,
            });
        }
        let remote = repo.find_remote(&name).map_err(error)?;
        let url = remote.url().map_err(error)?.to_owned();
        let refspecs = remote.fetch_refspecs().map_err(error)?;
        let fetch = refspecs
            .iter()
            .map(|spec| {
                spec.map_err(error)?
                    .map(str::to_owned)
                    .ok_or_else(|| error("remote refspec is not valid UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PullTarget {
            name,
            url,
            fetch,
            pull_rebase,
            branch_rebase,
            pull_ff,
        })
    }

    fn failure(error: BridgeError, completed: &[String], skipped: &[String]) -> BridgeError {
        if completed.is_empty() && skipped.is_empty() {
            return error;
        }
        let code = if error.code == "outcome_unknown" {
            "outcome_unknown"
        } else {
            "partial_failure"
        };
        BridgeError::new(
            code,
            format!(
                "Completed {} submodule(s): {}. Skipped {} submodule(s): {}. Remaining action failed: {}",
                completed.len(),
                completed.join(", "),
                skipped.len(),
                skipped.join(", "),
                error.message
            ),
        )
    }

    pub(super) fn run_batch(
        scope: &Scope,
        repo: &Path,
        input: &Request,
    ) -> Result<Value, BridgeError> {
        run_batch_scoped(scope, repo, input, None, Instant::now() + BATCH_TIMEOUT)
    }

    fn run_batch_scoped(
        scope: &Scope,
        repo: &Path,
        input: &Request,
        initial_heads: Option<&HashMap<PathBuf, ChildHead>>,
        deadline: Instant,
    ) -> Result<Value, BridgeError> {
        struct Frame {
            repo: PathBuf,
            prefix: PathBuf,
            head: ChildHead,
            entries: Vec<Entry>,
            index: usize,
            depth: usize,
        }

        let initial = entries(scope, repo, &input.operation)?;
        let mut completed = Vec::new();
        let mut skipped = Vec::new();
        let mut count = initial.len();
        let mut visited = HashSet::from([repo.to_path_buf()]);
        let mut frames = vec![Frame {
            repo: repo.to_path_buf(),
            prefix: PathBuf::new(),
            head: read_head(repo)?,
            entries: initial,
            index: 0,
            depth: 0,
        }];
        while let Some(mut frame) = frames.pop() {
            if frame.index >= frame.entries.len() {
                continue;
            }
            let index = frame.index;
            let entry = frame.entries[index].clone();
            frame.index += 1;
            let path = entry
                .path
                .to_str()
                .ok_or_else(|| error("submodule path is not valid UTF-8"))?
                .to_owned();
            let display = frame
                .prefix
                .join(&entry.path)
                .to_str()
                .ok_or_else(|| error("submodule path is not valid UTF-8"))?
                .to_owned();
            let checked = (|| {
                scope.repository(repo)?;
                mutation::verify_sync_head(
                    repo,
                    input.expected_branch.as_deref(),
                    &input.expected_head,
                )?;
                if scope.repository(&frame.repo)? != frame.repo
                    || read_head(&frame.repo)? != frame.head
                {
                    return Err(BridgeError::new(
                        "stale_target",
                        "Submodule parent branch or HEAD changed; refresh before retrying",
                    ));
                }
                let current = entries(scope, &frame.repo, &input.operation)?;
                if current.len() != frame.entries.len()
                    || current
                        .iter()
                        .map(|item| &item.path)
                        .ne(frame.entries.iter().map(|item| &item.path))
                    || current[index] != entry
                {
                    return Err(BridgeError::new(
                        "stale_target",
                        "Submodule configuration, remote, index, branch or HEAD changed; refresh before retrying",
                    ));
                }
                if let Some(initial_heads) = initial_heads {
                    if !entry.initialized {
                        return Ok(false);
                    }
                    let child = scope.repository(&frame.repo.join(&entry.path))?;
                    match initial_heads.get(&child) {
                        None => return Ok(false),
                        Some(head) if Some(head) != entry.child_head.as_ref() => {
                            return Err(BridgeError::new(
                                "stale_target",
                                "Submodule branch or HEAD changed during parent pull",
                            ));
                        }
                        Some(_) => {}
                    }
                }
                if entry.skip {
                    return Ok(false);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(BridgeError::new(
                        "timeout",
                        "Submodule action exceeded its time limit",
                    ));
                }
                let mut args: Vec<String> = [
                    "--literal-pathspecs",
                    "-c",
                    "submodule.recurse=false",
                    "-c",
                    "fetch.recurseSubmodules=false",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect();
                let target = frame.repo.join(&entry.path);
                let command_repo = if input.operation == "pull_latest" {
                    safe_path(scope, &frame.repo, &entry.path)?;
                    let child = scope.repository(&target)?;
                    let remote = entry
                        .pull_target
                        .as_ref()
                        .ok_or_else(|| error("submodule remote is unavailable"))?;
                    args.extend([
                        "pull".into(),
                        "--".into(),
                        remote.name.clone(),
                        "HEAD".into(),
                    ]);
                    child
                } else {
                    args.push("submodule".into());
                    if input.operation == "sync" {
                        args.push("sync".into());
                    } else {
                        let strategy = entry
                            .local_update
                            .as_deref()
                            .or(entry.module_update.as_deref())
                            .unwrap_or("checkout");
                        args.push("update".into());
                        if initial_heads.is_none() {
                            args.push("--init".into());
                        }
                        args.push(format!("--{strategy}"));
                    }
                    args.extend(["--".into(), path.clone()]);
                    frame.repo.clone()
                };
                let output = process::run_git_op_noninteractive(&command_repo, &args, remaining)
                    .map_err(|failure| {
                        let code = if failure.kind() == std::io::ErrorKind::NotFound {
                            "git_error"
                        } else {
                            "outcome_unknown"
                        };
                        BridgeError::new(code, format!("Git submodule action failed: {failure}"))
                    })?;
                if !output.status.success() {
                    let detail: String = String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .chars()
                        .take(2048)
                        .collect();
                    return Err(BridgeError::new(
                        "git_error",
                        format!("Git submodule action failed ({}): {detail}", output.status),
                    ));
                }
                if initial_heads.is_some() && scope.repository(&target)? != target {
                    return Err(BridgeError::new(
                        "stale_target",
                        "Submodule changed during update",
                    ));
                }
                Ok(true)
            })();
            let should_update = match checked {
                Ok(value) => value,
                Err(problem) => {
                    let problem = BridgeError::new(
                        problem.code,
                        format!(
                            "Submodule {display}: {}. The selected submodule may have changed; inspect it before retrying.",
                            problem.message
                        ),
                    );
                    return Err(failure(problem, &completed, &skipped));
                }
            };
            if !should_update {
                skipped.push(display);
                frames.push(frame);
                continue;
            }
            completed.push(display.clone());
            if input.operation == "update" {
                let child_result = (|| {
                    if Instant::now() >= deadline {
                        return Err(BridgeError::new(
                            "timeout",
                            "Submodule action exceeded its time limit",
                        ));
                    }
                    safe_path(scope, &frame.repo, &entry.path)?;
                    let child = scope.repository(&frame.repo.join(&entry.path))?;
                    if !visited.insert(child.clone()) {
                        return Err(BridgeError::new(
                            "invalid_request",
                            "Submodule cycle or duplicate detected",
                        ));
                    }
                    let nested = entries(scope, &child, &input.operation)?;
                    count = count
                        .checked_add(nested.len())
                        .ok_or_else(|| error("too many submodules"))?;
                    if count > MAX_SUBMODULES || (frame.depth >= MAX_DEPTH && !nested.is_empty()) {
                        return Err(BridgeError::new(
                            "invalid_request",
                            "Submodule nesting exceeds the batch limit",
                        ));
                    }
                    Ok(Frame {
                        repo: child.clone(),
                        prefix: frame.prefix.join(&entry.path),
                        head: read_head(&child)?,
                        entries: nested,
                        index: 0,
                        depth: frame.depth + 1,
                    })
                })();
                match child_result {
                    Ok(child) => {
                        frames.push(frame);
                        frames.push(child);
                    }
                    Err(problem) => return Err(failure(problem, &completed, &skipped)),
                }
            } else {
                frames.push(frame);
            }
        }
        if let Some(initial_heads) = initial_heads {
            for child in initial_heads
                .keys()
                .filter(|child| !visited.contains(*child))
            {
                let relative = child.strip_prefix(repo).map_err(error)?;
                let display = relative
                    .to_str()
                    .ok_or_else(|| error("submodule path is not valid UTF-8"))?
                    .to_owned();
                if !skipped.contains(&display) {
                    skipped.push(display);
                }
            }
            if completed.len() + skipped.len() > MAX_SUBMODULES {
                return Err(BridgeError::new(
                    "partial_failure",
                    "Too many submodules to report all skipped paths; inspect the repository",
                ));
            }
            skipped.sort();
        }
        Ok(json!({ "completed": completed, "skipped": skipped }))
    }

    pub(super) fn pull_with_submodules(
        scope: &Scope,
        repo: &Path,
        input: &Request,
    ) -> Result<Value, BridgeError> {
        let deadline = Instant::now() + BATCH_TIMEOUT;
        let root_head = read_head(repo)?;
        let branch = mutation::verify_sync_head(
            repo,
            input.expected_branch.as_deref(),
            &input.expected_head,
        )?;
        let children = scope.submodule_repositories(repo)?;
        if children.len() > MAX_SUBMODULES {
            return Err(BridgeError::new(
                "invalid_request",
                "too many initialized submodules for one action",
            ));
        }
        entries(scope, repo, "update")?;
        let mut initial_heads = HashMap::new();
        for child in children {
            entries(scope, &child, "update")?;
            initial_heads.insert(child.clone(), read_head(&child)?);
        }
        scope.repository(repo)?;
        mutation::verify_sync_head(repo, input.expected_branch.as_deref(), &input.expected_head)?;
        let args = crate::git::sync_arguments(
            repo,
            &branch,
            false,
            [
                "-c",
                "submodule.recurse=false",
                "-c",
                "fetch.recurseSubmodules=false",
                "pull",
                "--no-recurse-submodules",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        );
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BridgeError::new(
                "timeout",
                "Submodule action exceeded its time limit",
            ));
        }
        let output = process::run_git_op_noninteractive(repo, &args, remaining).map_err(|failure| {
            let code = if failure.kind() == std::io::ErrorKind::NotFound { "git_error" } else { "outcome_unknown" };
            BridgeError::new(code, format!("Parent pull did not complete: {failure}. Inspect the repository before retrying."))
        })?;
        if !output.status.success() {
            let detail: String = String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(2048)
                .collect();
            return Err(BridgeError::new(
                "git_error",
                format!(
                    "Parent pull failed ({}): {detail}. Inspect the repository before retrying.",
                    output.status
                ),
            ));
        }
        let result = (|| {
            if scope.repository(repo)? != repo {
                return Err(BridgeError::new(
                    "stale_target",
                    "Parent repository changed",
                ));
            }
            let head = read_head(repo)?;
            if head.branch_ref != root_head.branch_ref {
                return Err(BridgeError::new(
                    "stale_target",
                    "Parent branch changed during pull",
                ));
            }
            let expected_head = head
                .oid
                .ok_or_else(|| error("Parent HEAD is unavailable after pull"))?
                .to_string();
            let followup = Request {
                repo: repo.to_path_buf(),
                operation: "update".into(),
                expected_branch: input.expected_branch.clone(),
                expected_head,
            };
            run_batch_scoped(scope, repo, &followup, Some(&initial_heads), deadline)
        })()
        .map_err(|error| {
            BridgeError::new(
                if error.code == "outcome_unknown" {
                    "outcome_unknown"
                } else {
                    "partial_failure"
                },
                format!(
                    "Parent pull completed. Submodule update failed: {}",
                    error.message
                ),
            )
        })?;
        Ok(
            json!({ "parent_pulled": true, "completed": result["completed"], "skipped": result["skipped"] }),
        )
    }
}

#[cfg(unix)]
use unix::run_batch;

#[cfg(all(test, unix))]
#[path = "actions_tests.rs"]
mod tests;
