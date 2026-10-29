use std::io::Cursor;
use std::path::Path;

use git2::{Repository, Signature};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::config::SubmoduleConfig;

use super::{MAX_REQUEST_BYTES, serve};

fn create_repo(path: &Path) -> Repository {
    std::fs::create_dir_all(path).unwrap();
    let repo = Repository::init(path).unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

#[cfg(unix)]
fn add_submodule(repo: &Repository, source: &Path, path: &Path) {
    let mut submodule = repo
        .submodule(source.to_str().unwrap(), path, true)
        .unwrap();
    submodule.clone(None).unwrap();
    submodule.add_finalize().unwrap();
    drop(submodule);
    let mut index = repo.index().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "add submodule",
        &tree,
        &[&parent],
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn status_validates_nested_submodule_scope_and_reports_corrupt_child_index() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let parent_path = workspace.join("parent");
    let child_source = temporary.path().join("child-source");
    let nested_source = temporary.path().join("nested-source");
    create_repo(&nested_source);
    let source = create_repo(&child_source);
    add_submodule(&source, &nested_source, Path::new("nested"));
    let parent = create_repo(&parent_path);
    add_submodule(&parent, &child_source, Path::new("child"));
    let child_path = parent_path.join("child");
    let nested = child_path.join("nested");
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "status", "params": {"repo": parent_path}}),
    ];

    let result = exchange(&requests);
    assert!(result[1].get("result").is_some(), "{result:?}");

    if nested.exists() {
        std::fs::remove_dir(&nested).unwrap();
    }
    symlink(&nested_source, &nested).unwrap();
    let result = exchange(&requests);
    assert_eq!(
        result[1]["error"]["code"], "outside_workspace",
        "{result:?}"
    );
    std::fs::remove_file(&nested).unwrap();

    let child = Repository::open(&child_path).unwrap();
    let index = child.path().join("index");
    let original = std::fs::read(&index).unwrap();
    std::fs::write(&index, b"invalid index").unwrap();
    let result = exchange(&requests);
    assert_eq!(result[1]["error"]["code"], "repository_error", "{result:?}");
    std::fs::write(&index, original).unwrap();
    let result = exchange(&requests);
    assert!(result[1].get("result").is_some(), "{result:?}");
}

#[cfg(unix)]
#[test]
fn submodule_inspect_rejects_external_gitmodules_before_enumeration() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let parent = workspace.join("parent");
    create_repo(&parent);
    let external = temporary.path().join("modules");
    std::fs::write(
        &external,
        "[submodule \"child\"]\npath = child\nurl = example\n",
    )
    .unwrap();
    symlink(&external, parent.join(".gitmodules")).unwrap();
    let result = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "submodule_inspect", "params": {"repo": parent, "path": "child"}}),
    ]);
    assert_eq!(
        result[1]["error"]["code"], "outside_workspace",
        "{result:?}"
    );
}

fn add_worktree(repo: &Repository, name: &str, path: &Path) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let reference = repo.branch(name, &head, false).unwrap().into_reference();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repo.worktree(name, path, Some(&options)).unwrap();
}

fn exchange(requests: &[Value]) -> Vec<Value> {
    exchange_with_base(requests, None)
}

fn exchange_with_base(requests: &[Value], base: Option<&str>) -> Vec<Value> {
    let input = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect::<String>();
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        base,
        false,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn branch_review_uses_merge_base_and_respects_workspace_and_configured_base() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let outside = temporary.path().join("outside");
    let repo = create_repo(&repo_path);
    create_repo(&outside);
    let initial = repo.head().unwrap().peel_to_commit().unwrap();
    let initial_id = initial.id();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let base_tree = initial.tree().unwrap();
    let base_id = repo
        .commit(
            None,
            &signature,
            &signature,
            "base diverged",
            &base_tree,
            &[&initial],
        )
        .unwrap();
    repo.reference("refs/remotes/origin/main", base_id, true, "test")
        .unwrap();
    std::fs::write(repo_path.join("branch.txt"), "branch\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("branch.txt")).unwrap();
    index.write().unwrap();
    let branch_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let head_id = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "branch",
            &branch_tree,
            &[&initial],
        )
        .unwrap();
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "branch_review", "params": {"repo": repo_path}}),
        json!({"id": 3, "method": "branch_review", "params": {"repo": outside}}),
    ];
    let replies = exchange(&requests);
    assert_eq!(replies[1]["result"]["base_ref"], "origin/main");
    assert_eq!(replies[1]["result"]["base_oid"], initial_id.to_string());
    assert_eq!(replies[1]["result"]["head_oid"], head_id.to_string());
    assert_eq!(
        replies[1]["result"]["files"],
        json!([{"status": "A", "path": "branch.txt", "is_submodule": false}])
    );
    assert_eq!(replies[2]["error"]["code"], "outside_workspace");
    let configured = exchange_with_base(&requests, Some("HEAD"));
    assert_eq!(configured[1]["result"]["base_oid"], head_id.to_string());
    assert_eq!(configured[1]["result"]["files"], json!([]));
    let invalid = exchange_with_base(&requests, Some("missing-reference"));
    assert_eq!(invalid[1]["error"]["code"], "repository_error");
}

#[test]
fn commit_and_branch_review_include_both_paths_for_renamed_files() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let initial = repo.head().unwrap().peel_to_commit().unwrap();
    std::fs::write(repo_path.join("old.txt"), "some distinctive content\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("old.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let base = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "base",
            &tree,
            &[&initial],
        )
        .unwrap();
    std::fs::rename(repo_path.join("old.txt"), repo_path.join("new.txt")).unwrap();
    index.remove_path(Path::new("old.txt")).unwrap();
    index.add_path(Path::new("new.txt")).unwrap();
    index.write().unwrap();
    let new_tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.find_commit(base).unwrap();
    let head = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "rename",
            &new_tree,
            &[&parent],
        )
        .unwrap();
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "commit_files", "params": {"repo": repo_path, "oid": head.to_string()}}),
        json!({"id": 3, "method": "branch_review", "params": {"repo": repo_path}}),
    ];
    let replies = exchange_with_base(&requests, Some(&base.to_string()));
    let expected =
        json!([{"status": "R", "path": "new.txt", "old_path": "old.txt", "is_submodule": false}]);
    assert_eq!(replies[1]["result"]["files"], expected);
    assert_eq!(replies[2]["result"]["files"], expected);
}

#[test]
fn status_and_graph_use_workspace_repositories_and_canonical_git_logic() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace with spaces");
    let repo_path = workspace.join("repo");
    let repo = create_repo(&repo_path);
    std::fs::write(repo_path.join("new file.txt"), "untracked").unwrap();
    let inside = workspace.join("inside-worktree");
    let outside = temporary.path().join("outside-worktree");
    add_worktree(&repo, "inside-branch", &inside);
    add_worktree(&repo, "outside-branch", &outside);
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "status", "params": {"repo": repo_path}}),
        json!({"id": 3, "method": "graph", "params": {"repo": repo_path}}),
    ]);
    assert_eq!(replies[0], json!({"id": 1, "result": {"version": 1}}));
    let status = &replies[1]["result"];
    assert_eq!(status["is_dirty"], true);
    assert!(
        status["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| { file["path"] == "new file.txt" && file["status"] == "untracked" })
    );
    let worktrees = status["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0]["branch"], "inside-branch");
    let graph = replies[2]["result"]["rows"].as_array().unwrap();
    let labels = graph
        .iter()
        .flat_map(|row| row["labels"].as_array().unwrap());
    let flags: std::collections::HashMap<_, _> = labels
        .map(|label| {
            (
                label["name"].as_str().unwrap(),
                label["is_worktree"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(flags.get("inside-branch"), Some(&true));
    assert_eq!(flags.get("outside-branch"), Some(&false));
    assert_eq!(graph[0]["message"], "initial");
    assert_eq!(graph[0]["diff_stat"]["additions"], 0);
}

#[test]
fn graph_filters_branches_authors_and_refs_without_losing_branch_choices() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let root = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = root.tree().unwrap();
    let alice = Signature::now("Alice", "alice@example.invalid").unwrap();
    let bob = Signature::now("Bob", "bob@example.invalid").unwrap();
    let feature = repo
        .commit(None, &alice, &alice, "feature", &tree, &[&root])
        .unwrap();
    repo.reference("refs/heads/feature", feature, true, "fixture")
        .unwrap();
    repo.commit(Some("HEAD"), &bob, &bob, "main", &tree, &[&root])
        .unwrap();
    repo.tag_lightweight("v1", root.as_object(), false).unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "graph", "params": {"repo": repo_path}}),
        json!({"id": 3, "method": "graph", "params": {"repo": repo_path, "filters": {"branches": ["feature"]}}}),
        json!({"id": 4, "method": "graph", "params": {"repo": repo_path, "filters": {"authors": ["Alice"]}}}),
        json!({"id": 5, "method": "graph", "params": {"repo": repo_path, "filters": {"branches": []}}}),
        json!({"id": 6, "method": "graph", "params": {"repo": repo_path, "filters": {"refs": {"local": false, "remote": false}}}}),
        json!({"id": 7, "method": "graph", "params": {"repo": repo_path, "filters": {"refs": {"unexpected": true}}}}),
    ]);
    let messages = |index: usize| {
        replies[index]["result"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["message"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert!(messages(1).contains(&"main") && messages(1).contains(&"feature"));
    assert!(messages(2).contains(&"feature") && !messages(2).contains(&"main"));
    assert_eq!(messages(3), ["feature"]);
    assert!(messages(4).is_empty());
    assert!(
        replies[4]["result"]["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "feature")
    );
    assert_eq!(messages(5), ["initial"]);
    assert!(
        replies[5]["result"]["rows"][0]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|label| label["is_tag"] == true)
    );
    assert_eq!(replies[6]["error"]["code"], "invalid_request");
}

#[test]
fn graph_collapses_and_expands_the_canonical_side_branch_segment() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let root = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = root.tree().unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let first_oid = repo
        .commit(
            None,
            &signature,
            &signature,
            "feature first",
            &tree,
            &[&root],
        )
        .unwrap();
    let first = repo.find_commit(first_oid).unwrap();
    let tip_oid = repo
        .commit(
            None,
            &signature,
            &signature,
            "feature tip",
            &tree,
            &[&first],
        )
        .unwrap();
    let tip = repo.find_commit(tip_oid).unwrap();
    repo.branch("feature", &tip, false).unwrap();
    let main_oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "main",
            &tree,
            &[&root],
        )
        .unwrap();
    let main = repo.find_commit(main_oid).unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "merge",
        &tree,
        &[&main, &tip],
    )
    .unwrap();
    let initialize = json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}});
    let read = |collapsed: Vec<String>| {
        exchange(&[
            initialize.clone(),
            json!({"id": 2, "method": "graph", "params": {"repo": repo_path, "collapsed_segments": collapsed}}),
        ])
    };
    let expanded = read(Vec::new());
    let rows = expanded[1]["result"]["rows"].as_array().unwrap();
    let feature = rows
        .iter()
        .find(|row| row["oid"] == tip_oid.to_string())
        .unwrap();
    let segment = feature["segment_id"].as_str().unwrap().to_owned();
    assert_eq!(segment, tip_oid.to_string());
    let root_row = rows
        .iter()
        .find(|row| row["oid"] == root.id().to_string())
        .unwrap();
    assert!(root_row["segment_id"].is_null());
    let collapsed = read(vec![segment.clone()]);
    let collapsed_rows = collapsed[1]["result"]["rows"].as_array().unwrap();
    let placeholder = collapsed_rows
        .iter()
        .find(|row| row["collapsed"].is_object())
        .unwrap();
    assert_eq!(placeholder["oid"], tip_oid.to_string());
    assert_eq!(placeholder["segment_id"], segment);
    assert_eq!(placeholder["collapsed"], json!({"id": segment, "count": 2}));
    assert_eq!(placeholder["message"], "▶ feature (2 commits)");
    assert!(placeholder["diff_stat"].is_null());
    assert!(
        !collapsed_rows
            .iter()
            .any(|row| row["oid"] == first_oid.to_string())
    );
    assert_eq!(collapsed_rows.len() + 1, rows.len());
    let ignored = read(vec!["nonexistent".into()]);
    assert_eq!(ignored[1]["result"]["rows"], expanded[1]["result"]["rows"]);
    assert_eq!(
        read(Vec::new())[1]["result"]["rows"],
        expanded[1]["result"]["rows"]
    );
}

#[test]
fn scoped_status_reports_a_linked_worktree_with_an_invalid_index() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = create_repo(&repo_path);
    let worktree_path = workspace.join("linked");
    add_worktree(&repo, "linked-branch", &worktree_path);
    let worktree = Repository::open(&worktree_path).unwrap();
    std::fs::write(worktree.path().join("index"), b"invalid index").unwrap();

    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "status", "params": {"repo": repo_path}}),
    ]);
    assert_eq!(replies[1]["error"]["code"], "repository_error");
    assert!(crate::git::status::query_status(&repo_path, &SubmoduleConfig::default()).is_ok());
}

#[test]
fn commit_files_returns_root_and_first_parent_merge_changes_within_scope() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = create_repo(&repo_path);
    let initial = repo.head().unwrap().peel_to_commit().unwrap();
    std::fs::write(repo_path.join("root.txt"), "content").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("root.txt")).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let root_oid = repo
        .commit(None, &signature, &signature, "Root", &tree, &[])
        .unwrap();
    let second_parent = repo.find_commit(root_oid).unwrap();
    let merge_oid = repo
        .commit(
            None,
            &signature,
            &signature,
            "Merge",
            &tree,
            &[&initial, &second_parent],
        )
        .unwrap();
    let outside = temporary.path().join("outside");
    create_repo(&outside);
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "commit_files", "params": {"repo": repo_path, "oid": root_oid.to_string()}}),
        json!({"id": 3, "method": "commit_files", "params": {"repo": repo_path, "oid": merge_oid.to_string()}}),
        json!({"id": 4, "method": "commit_files", "params": {"repo": outside, "oid": root_oid.to_string()}}),
        json!({"id": 5, "method": "commit_files", "params": {"repo": repo_path, "oid": "invalid"}}),
    ]);
    assert_eq!(
        replies[1]["result"],
        json!({"message": "Root", "files": [{"status": "A", "path": "root.txt", "is_submodule": false}]})
    );
    assert_eq!(
        replies[2]["result"],
        json!({"message": "Merge", "files": [{"status": "A", "path": "root.txt", "is_submodule": false}]})
    );
    assert_eq!(replies[3]["error"]["code"], "outside_workspace");
    assert_eq!(replies[4]["error"]["code"], "repository_error");
}

#[test]
fn commit_files_and_graph_report_missing_parent_instead_of_treating_it_as_root() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let tree = repo.head().unwrap().peel_to_tree().unwrap().id();
    let contents = format!(
        "tree {tree}\nparent 0000000000000000000000000000000000000001\nauthor Test <test@example.invalid> 1 +0000\ncommitter Test <test@example.invalid> 1 +0000\n\nMissing parent\n"
    );
    let oid = repo
        .odb()
        .unwrap()
        .write(git2::ObjectType::Commit, contents.as_bytes())
        .unwrap();
    repo.reference("refs/heads/missing-parent", oid, true, "test")
        .unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "commit_files", "params": {"repo": repo_path, "oid": oid.to_string()}}),
        json!({"id": 3, "method": "graph", "params": {"repo": repo_path}}),
    ]);
    assert_eq!(replies[1]["error"]["code"], "repository_error");
    assert_eq!(replies[2]["error"]["code"], "repository_error");
}

#[test]
fn commit_files_identifies_gitlink_entries() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("child", head.id(), 0o160000).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let oid = repo
        .commit(None, &signature, &signature, "Submodule", &tree, &[])
        .unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "commit_files", "params": {"repo": repo_path, "oid": oid.to_string()}}),
    ]);
    assert_eq!(
        replies[1]["result"]["files"],
        json!([{"status": "A", "path": "child", "is_submodule": true}])
    );
}

#[cfg(unix)]
#[test]
fn commit_files_rejects_non_utf8_paths_instead_of_rewriting_them() {
    let temporary = TempDir::new().unwrap();
    let repo_path = temporary.path().join("repo");
    let repo = create_repo(&repo_path);
    let blob = repo.blob(b"content").unwrap();
    let mut raw_tree = b"100644 bad-\xff\0".to_vec();
    raw_tree.extend_from_slice(blob.as_bytes());
    let tree_oid = repo
        .odb()
        .unwrap()
        .write(git2::ObjectType::Tree, &raw_tree)
        .unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let oid = repo
        .commit(None, &signature, &signature, "Non UTF-8", &tree, &[])
        .unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [temporary.path()]}}),
        json!({"id": 2, "method": "commit_files", "params": {"repo": repo_path, "oid": oid.to_string()}}),
    ]);
    assert_eq!(replies[1]["error"]["code"], "repository_error");
    assert!(
        replies[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("UTF-8")
    );
}

#[test]
fn initialization_replaces_scope_and_rejects_outside_or_non_root_repositories() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo = workspace.join("repo");
    let outside = temporary.path().join("workspace-sibling");
    create_repo(&repo);
    create_repo(&outside);
    let subdir = repo.join("nested");
    std::fs::create_dir(&subdir).unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "status", "params": {"repo": repo}}),
        json!({"id": 2, "method": "initialize", "params": {"version": 9, "workspace_roots": [workspace]}}),
        json!({"id": 3, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 4, "method": "status", "params": {"repo": outside}}),
        json!({"id": 5, "method": "status", "params": {"repo": subdir}}),
        json!({"id": 6, "method": "initialize", "params": {"version": 1, "workspace_roots": []}}),
        json!({"id": 7, "method": "graph", "params": {"repo": repo}}),
    ]);
    assert_eq!(replies[0]["error"]["code"], "not_initialized");
    assert_eq!(replies[1]["error"]["code"], "unsupported_version");
    assert_eq!(replies[2]["result"]["version"], 1);
    assert_eq!(replies[3]["error"]["code"], "outside_workspace");
    assert!(replies[4]["error"].is_object());
    assert_eq!(replies[5]["result"]["version"], 1);
    assert_eq!(replies[6]["error"]["code"], "outside_workspace");
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_make_an_outside_repository_part_of_a_workspace() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = temporary.path().join("outside");
    create_repo(&outside);
    let link = workspace.join("linked");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let replies = exchange(&[
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "status", "params": {"repo": link}}),
    ]);
    assert_eq!(replies[1]["error"]["code"], "outside_workspace");
}

#[cfg(unix)]
#[test]
fn status_rejects_submodules_outside_workspace_and_accepts_missing_inside() {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    let repo_path = workspace.join("repo");
    let repo = create_repo(&repo_path);
    let config = repo_path.join(".gitmodules");
    std::fs::write(
        &config,
        "[submodule \"child\"]\n\tpath = deps/missing\n\turl = example\n",
    )
    .unwrap();
    assert_eq!(repo.submodules().unwrap().len(), 1);
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": [workspace]}}),
        json!({"id": 2, "method": "status", "params": {"repo": repo_path}}),
    ];
    let replies = exchange(&requests);
    assert!(replies[1].get("result").is_some(), "{:?}", replies[1]);

    let outside = temporary.path().join("outside-submodule");
    create_repo(&outside);
    std::os::unix::fs::symlink(&outside, repo_path.join("linked")).unwrap();
    std::fs::write(
        &config,
        "[submodule \"child\"]\n\tpath = linked\n\turl = example\n",
    )
    .unwrap();
    let replies = exchange(&requests);
    assert_eq!(replies[1]["error"]["code"], "outside_workspace");
}

#[test]
fn malformed_and_oversized_requests_return_errors() {
    let mut input = b"not json\n".to_vec();
    input.extend(vec![b'x'; MAX_REQUEST_BYTES as usize + 1]);
    let mut output = Vec::new();
    serve(
        &mut Cursor::new(input),
        &mut output,
        &SubmoduleConfig::default(),
        None,
        false,
    )
    .unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["error"]["code"], "invalid_request");
    assert_eq!(replies[1]["error"]["code"], "request_too_large");
}
