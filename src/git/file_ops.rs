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
    if let Some(text) = conflict_diff(&repo, selected)? {
        return Ok(text);
    }
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

pub(crate) fn conflict_diff(
    repo: &git2::Repository,
    selected: &Path,
) -> color_eyre::Result<Option<String>> {
    let index = repo.index()?;
    let base = index.get_path(selected, 1);
    let ours = index.get_path(selected, 2);
    let theirs = index.get_path(selected, 3);
    if base.is_none() && ours.is_none() && theirs.is_none() {
        return Ok(None);
    }

    let missing = if base.is_some() { "deleted" } else { "absent" };
    let describe = |entry: Option<&git2::IndexEntry>| match entry {
        Some(entry) if entry.mode == 0o100644 => entry.id.to_string()[..7].to_owned(),
        Some(entry) => format!("{} (mode {:06o})", &entry.id.to_string()[..7], entry.mode),
        None => missing.to_owned(),
    };
    let mut text = format!(
        "Merge conflict\n- Ours (Current): {}\n+ Theirs (Incoming): {}\n  Base: {}\n\n",
        describe(ours.as_ref()),
        describe(theirs.as_ref()),
        base.as_ref()
            .map_or_else(|| "absent".into(), |entry| describe(Some(entry))),
    );
    let content = |entry: Option<&git2::IndexEntry>| -> color_eyre::Result<Vec<u8>> {
        let Some(entry) = entry else {
            return Ok(Vec::new());
        };
        if entry.mode == 0o160000 {
            return Ok(format!("Subproject commit {}\n", entry.id).into_bytes());
        }
        Ok(repo.find_blob(entry.id)?.content().to_vec())
    };
    let old = content(ours.as_ref())?;
    let new = content(theirs.as_ref())?;
    let mut options = git2::DiffOptions::new();
    options.old_prefix("ours/").new_prefix("theirs/");
    let mut patch = git2::Patch::from_buffers(
        &old,
        Some(selected),
        &new,
        Some(selected),
        Some(&mut options),
    )?;
    let patch = patch.to_buf()?;
    if patch.is_empty() {
        text.push_str("(no content differences)");
    } else {
        text.push_str(&String::from_utf8_lossy(&patch));
    }
    Ok(Some(text))
}

pub(crate) fn merge_tool(
    repo_path: &Path,
    selected: &Path,
) -> color_eyre::Result<std::process::Command> {
    // Git's mergetool script expands its unquoted file list after selecting
    // paths. Literal pathspecs alone cannot prevent a second expansion.
    if !selected.to_str().is_some_and(|path| {
        !path
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '*' | '?' | '[' | '"' | '\\'))
    }) {
        return Err(color_eyre::eyre::eyre!(
            "Git mergetool cannot safely open this filename"
        ));
    }
    let repo = git2::Repository::open(repo_path)?;
    let index = repo.index()?;
    let mut stages = (1..=3)
        .filter_map(|stage| index.get_path(selected, stage))
        .peekable();
    if stages.peek().is_none() || stages.any(|entry| entry.mode == 0o160000) {
        return Err(color_eyre::eyre::eyre!(
            "The selected file has no supported merge conflict"
        ));
    }
    let mut command = super::process::git_command(repo_path);
    command
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["mergetool", "--"])
        .arg(selected);
    Ok(command)
}

#[cfg(test)]
mod tests;
