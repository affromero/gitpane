use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::git::repo_sort::{self, Key, Order};

use super::{BridgeError, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    path: PathBuf,
    display: String,
    dirty: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    order: String,
    repositories: Vec<Entry>,
}

pub(super) fn sort(scope: &Scope, request: Request) -> Result<Value, BridgeError> {
    let order = match request.order.as_str() {
        "alphabetical" => Order::Alphabetical,
        "reverse" => Order::ReverseAlphabetical,
        "dirty" => Order::DirtyFirst,
        _ => {
            return Err(BridgeError::new(
                "invalid_request",
                "unknown repository sort order",
            ));
        }
    };
    if request.repositories.len() > 4096 {
        return Err(BridgeError::new(
            "invalid_request",
            "too many repositories to sort",
        ));
    }
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for entry in &request.repositories {
        if entry.display.len() > 4096 || entry.display.contains('\0') {
            return Err(BridgeError::new(
                "invalid_request",
                "invalid repository display name",
            ));
        }
        let path = scope.repository(&entry.path)?;
        if !seen.insert(path.clone()) {
            return Err(BridgeError::new("invalid_request", "duplicate repository"));
        }
        paths.push(path);
    }
    let by_path: HashMap<_, _> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (path.as_path(), index))
        .collect();
    let mut keys = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let mut anchor = index;
        while paths[anchor].join(".git").is_file() {
            let Some(parent) = paths[anchor]
                .ancestors()
                .skip(1)
                .find_map(|path| by_path.get(path).copied())
            else {
                break;
            };
            anchor = parent;
        }
        keys.push(Key {
            anchor_clean: !request.repositories[anchor].dirty,
            anchor: request.repositories[anchor].display.to_lowercase(),
            nested: path != &paths[anchor],
            own: request.repositories[index].display.to_lowercase(),
        });
    }
    let mut indices: Vec<usize> = (0..paths.len()).collect();
    indices.sort_by(|a, b| repo_sort::compare(&keys[*a], &keys[*b], order));
    for path in &paths {
        scope.repository(path)?;
    }
    Ok(json!({"indices": indices}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorting_preserves_nested_gitlink_groups_and_rejects_outside_repositories() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let parent_path = workspace.join("z-parent");
        let other_path = workspace.join("a-other");
        std::fs::create_dir_all(&parent_path).unwrap();
        std::fs::create_dir_all(&other_path).unwrap();
        let parent = git2::Repository::init(&parent_path).unwrap();
        git2::Repository::init(&other_path).unwrap();
        let tree = parent
            .find_tree(parent.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        parent
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        let child_path = parent_path.join("a-child");
        let head = parent.head().unwrap().peel_to_commit().unwrap();
        let reference = parent
            .branch("child", &head, false)
            .unwrap()
            .into_reference();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&reference));
        parent
            .worktree("child", &child_path, Some(&options))
            .unwrap();
        let scope = Scope::new(vec![workspace]).unwrap();
        let make = |order: &str| Request {
            order: order.into(),
            repositories: vec![
                Entry {
                    path: child_path.clone(),
                    display: "a-child".into(),
                    dirty: true,
                },
                Entry {
                    path: parent_path.clone(),
                    display: "z-parent".into(),
                    dirty: false,
                },
                Entry {
                    path: other_path.clone(),
                    display: "a-other".into(),
                    dirty: true,
                },
            ],
        };
        assert_eq!(
            sort(&scope, make("alphabetical")).unwrap()["indices"],
            json!([2, 1, 0])
        );
        assert_eq!(
            sort(&scope, make("reverse")).unwrap()["indices"],
            json!([1, 0, 2])
        );
        assert_eq!(
            sort(&scope, make("dirty")).unwrap()["indices"],
            json!([2, 1, 0])
        );
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        git2::Repository::init(&outside).unwrap();
        let mut request = make("alphabetical");
        request.repositories.push(Entry {
            path: outside,
            display: "outside".into(),
            dirty: false,
        });
        assert_eq!(sort(&scope, request).unwrap_err().code, "outside_workspace");
    }
}
