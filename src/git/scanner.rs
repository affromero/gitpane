use crate::config::Config;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Cheap check that a `.git` directory looks real. A bare `mkdir .git` (which
/// can happen by accident, e.g. an aborted clone) is enough to fool a
/// `.git.exists()` test but `Repository::open` then fails on every status
/// query, so we treat such paths as not-a-repo at discovery time. Anything
/// produced by `git init` will have a `HEAD` file; that is what we look for.
fn is_real_git_dir(dot_git: &Path) -> bool {
    dot_git.join("HEAD").is_file()
}

pub(crate) fn discover_repos(config: &Config) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut repos = Vec::new();

    // Pinned repos first
    for pinned in &config.pinned_repos {
        let canonical = pinned.canonicalize().unwrap_or_else(|_| pinned.clone());
        if is_real_git_dir(&canonical.join(".git")) && seen.insert(canonical.clone()) {
            repos.push(canonical);
        }
    }

    // Discover from root dirs
    for root in &config.root_dirs {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root)
            .max_depth(config.scan_depth)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == ".git"
                && entry.file_type().is_dir()
                && is_real_git_dir(entry.path())
            {
                let repo_path = entry
                    .path()
                    .parent()
                    .unwrap()
                    .canonicalize()
                    .unwrap_or_else(|_| entry.path().parent().unwrap().to_path_buf());

                // Check exclusions
                let repo_name = repo_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let path_str = repo_path.to_string_lossy();
                let excluded = config
                    .excluded_repos
                    .iter()
                    .any(|pattern| repo_name == *pattern || path_str.contains(pattern));

                if !excluded && seen.insert(repo_path.clone()) {
                    repos.push(repo_path);
                }
            }
        }
    }

    repos.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(&b.file_name().unwrap_or_default().to_ascii_lowercase())
    });

    // Re-prepend pinned repos at the top (they were sorted away)
    let pinned_set: HashSet<PathBuf> = config
        .pinned_repos
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();

    if !pinned_set.is_empty() {
        let mut pinned: Vec<PathBuf> = repos
            .iter()
            .filter(|r| pinned_set.contains(*r))
            .cloned()
            .collect();
        let rest: Vec<PathBuf> = repos
            .into_iter()
            .filter(|r| !pinned_set.contains(r))
            .collect();
        pinned.extend(rest);
        repos = pinned;
    }

    repos
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_repo(parent: &std::path::Path, name: &str) -> PathBuf {
        let repo_dir = parent.join(name);
        let dot_git = repo_dir.join(".git");
        fs::create_dir_all(&dot_git).unwrap();
        // Mimic `git init`: a HEAD file is the minimum is_real_git_dir checks.
        fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        repo_dir
    }

    /// Empty `.git` directory with no HEAD file — what an aborted clone or a
    /// stray `mkdir .git` looks like. Discovery must NOT pick this up because
    /// `Repository::open` will fail on every status query, which the watcher
    /// then loops on, producing infinite red error toasts in the status bar.
    fn make_phantom_git_dir(parent: &std::path::Path, name: &str) -> PathBuf {
        let repo_dir = parent.join(name);
        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        repo_dir
    }

    #[test]
    fn test_discover_finds_git_repos() {
        let tmp = TempDir::new().unwrap();
        make_repo(tmp.path(), "alpha");
        make_repo(tmp.path(), "beta");

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn test_excluded_repos_are_filtered() {
        let tmp = TempDir::new().unwrap();
        make_repo(tmp.path(), "good-repo");
        make_repo(tmp.path(), "node_modules");

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            excluded_repos: vec!["node_modules".into()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1);
        assert!(repos[0].ends_with("good-repo"));
    }

    #[test]
    fn test_discover_skips_phantom_dot_git_at_root() {
        // Reproduces the gcloud-h100 case: `~/Code/.git/` exists as an empty
        // dir (no HEAD), and `~/Code` is the configured root_dir. Without
        // the HEAD check, discover_repos would emit `~/Code` itself as a
        // repo, then every file change anywhere under `~/Code` would route
        // to it via the watcher's classifier, `Repository::open` would fail,
        // and we'd surface a Failed-to-query toast per event.
        let tmp = TempDir::new().unwrap();
        make_phantom_git_dir(tmp.path(), ""); // creates tmp/.git (empty)
        make_repo(tmp.path(), "real-repo");

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("real-repo"));
    }

    #[test]
    fn test_discover_skips_phantom_dot_git_in_child() {
        let tmp = TempDir::new().unwrap();
        make_phantom_git_dir(tmp.path(), "broken");
        make_repo(tmp.path(), "ok");

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("ok"));
    }

    #[test]
    fn test_pinned_phantom_repo_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let phantom = make_phantom_git_dir(tmp.path(), "phantom");
        let real = make_repo(tmp.path(), "real");

        let config = Config {
            root_dirs: vec![],
            pinned_repos: vec![phantom, real.clone()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("real"));
    }

    #[test]
    fn test_pinned_repos_appear_first() {
        let tmp = TempDir::new().unwrap();
        let z_repo = make_repo(tmp.path(), "z-repo");
        make_repo(tmp.path(), "a-repo");

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            pinned_repos: vec![z_repo.clone()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 2);
        assert!(repos[0].ends_with("z-repo"));
    }
}
