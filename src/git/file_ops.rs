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
    let mut words = match operation {
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
        if matches!(operation, FileOperation::Discard) {
            match repo.head() {
                Ok(_) => {}
                Err(error) if error.code() == git2::ErrorCode::UnbornBranch => {
                    if !repo.status_file(selected)?.is_index_new() {
                        return Err(color_eyre::eyre::eyre!(
                            "Cannot discard a path that is not a staged addition before the first commit"
                        ));
                    }
                    words = vec!["rm", "-f"];
                }
                Err(error) => return Err(error.into()),
            }
        }
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
    let untracked = repo_path.join(selected).is_dir() || repo.status_file(selected)?.is_wt_new();
    if untracked {
        let mut options = git2::DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true)
            .disable_pathspec_match(true)
            .pathspec(selected);
        let diff = repo.diff_index_to_workdir(None, Some(&mut options))?;
        let mut text = String::new();
        super::commit_files::diff_to_string(&diff, &mut text)?;
        return Ok(if text.is_empty() {
            "(no diff available)".into()
        } else {
            text
        });
    }
    let mut command = super::process::git_command(repo_path);
    command.current_dir(repo_path);
    let mut args = arguments(repo_path, selected, FileOperation::Diff)?;
    match repo.head() {
        Ok(_) => {}
        Err(error) if error.code() == git2::ErrorCode::UnbornBranch => {
            // An unborn branch has an index but no HEAD tree yet.
            args[2] = "--cached".to_owned();
        }
        Err(error) => return Err(error.into()),
    }
    command.args(args);
    let output = command.output()?;
    if !output.status.success() {
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
