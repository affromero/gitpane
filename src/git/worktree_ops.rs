use std::path::Path;

pub(crate) fn create_arguments(branch: &str, path: &Path) -> Vec<String> {
    vec![
        "worktree".into(),
        "add".into(),
        "-b".into(),
        branch.into(),
        "--".into(),
        path.to_string_lossy().into_owned(),
    ]
}

pub(crate) fn remove_arguments(path: &Path) -> Vec<String> {
    vec![
        "worktree".into(),
        "remove".into(),
        "--".into(),
        path.to_string_lossy().into_owned(),
    ]
}
