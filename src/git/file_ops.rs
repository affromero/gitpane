use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) enum FileOperation {
    Stage,
    Unstage,
    Discard,
    DeleteUntracked,
    Diff,
}

pub(crate) fn arguments(
    repo_path: &Path,
    selected: &Path,
    operation: FileOperation,
) -> color_eyre::Result<Vec<String>> {
    let words = match operation {
        FileOperation::Stage => vec!["add", "-A"],
        FileOperation::Unstage => vec!["reset", "-q"],
        FileOperation::Discard => vec!["restore", "--staged", "--worktree"],
        FileOperation::DeleteUntracked => vec!["clean", "-fdq"],
        FileOperation::Diff => vec!["diff", "HEAD"],
    };
    let mut paths = vec![selected.to_path_buf()];
    if !matches!(
        operation,
        FileOperation::DeleteUntracked | FileOperation::Stage
    ) {
        let repo = git2::Repository::open(repo_path)?;
        let mut options = git2::StatusOptions::new();
        options.include_untracked(true).renames_head_to_index(true);
        let statuses = repo.statuses(Some(&mut options))?;
        for entry in statuses.iter() {
            let Some(delta) = entry.head_to_index() else {
                continue;
            };
            if delta.status() != git2::Delta::Renamed || delta.new_file().path() != Some(selected) {
                continue;
            }
            if let Some(old) = delta.old_file().path() {
                if matches!(operation, FileOperation::Discard) {
                    match std::fs::symlink_metadata(repo_path.join(old)) {
                        Ok(_) => {
                            return Err(color_eyre::eyre::eyre!(
                                "Cannot discard rename: '{}' already exists. Move it aside first to preserve its contents.",
                                old.display()
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                paths.push(old.to_path_buf());
            }
        }
    }
    let mut args = vec!["--literal-pathspecs".to_owned()];
    args.extend(words.into_iter().map(str::to_owned));
    args.push("--".into());
    for path in paths {
        let value = path.to_str().ok_or_else(|| {
            color_eyre::eyre::eyre!("Cannot run file operation on a non-UTF-8 path")
        })?;
        args.push(value.to_owned());
    }
    Ok(args)
}

pub(crate) fn diff(repo_path: &Path, selected: &Path) -> color_eyre::Result<String> {
    let repo = git2::Repository::open(repo_path)?;
    let untracked = repo.status_file(selected)?.is_wt_new();
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo_path).current_dir(repo_path);
    if untracked {
        command
            .args(["diff", "--no-index", "--", "/dev/null"])
            .arg(selected);
    } else {
        let mut args = arguments(repo_path, selected, FileOperation::Diff)?;
        if repo.head().is_err() {
            // An unborn branch has an index but no HEAD tree yet.
            if !repo.is_empty()? {
                return Err(color_eyre::eyre::eyre!("Cannot resolve repository HEAD"));
            }
            args[2] = "--cached".to_owned();
        }
        command.args(args);
    }
    let output = command.output()?;
    if !(output.status.success() || untracked && output.status.code() == Some(1)) {
        return Err(color_eyre::eyre::eyre!(
            "{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(if text.is_empty() {
        "(no diff available)".into()
    } else {
        text
    })
}

#[cfg(test)]
mod tests;
