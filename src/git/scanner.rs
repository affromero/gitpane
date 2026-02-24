use crate::config::Config;
use std::collections::HashSet;
use std::path::PathBuf;
use walkdir::WalkDir;

pub(crate) fn discover_repos(config: &Config) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut repos = Vec::new();

    // Pinned repos first
    for pinned in &config.pinned_repos {
        let canonical = pinned.canonicalize().unwrap_or_else(|_| pinned.clone());
        if canonical.join(".git").exists() && seen.insert(canonical.clone()) {
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
            if entry.file_name() == ".git" && entry.file_type().is_dir() {
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
