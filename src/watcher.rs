use notify_debouncer_full::{
    DebounceEventResult, Debouncer, NoCache, new_debouncer_opt,
    notify::{Config, RecommendedWatcher, RecursiveMode},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

pub(crate) struct RepoWatcher {
    _debouncer: Debouncer<RecommendedWatcher, NoCache>,
}

/// How a single filesystem event is attributed.
#[derive(Debug, PartialEq, Eq)]
enum Classification {
    /// The change happened inside this known repo; emit `RepoChanged`.
    Repo(PathBuf),
    /// The change happened at the top level of a configured root dir but
    /// outside any known repo (e.g. a new clone). Emit `ReposRootChanged`.
    RootDir,
    /// Excluded, noise, or unrelated — drop.
    Ignore,
}

/// Pure routing logic, extracted so it can be unit-tested without a real
/// filesystem watcher.
fn classify(
    changed_path: &Path,
    repo_paths: &[PathBuf],
    root_dirs: &[PathBuf],
    exclude_set: &HashSet<String>,
) -> Classification {
    // Skip events from excluded directories (node_modules, target, etc.).
    if changed_path
        .components()
        .any(|c| exclude_set.contains(c.as_os_str().to_string_lossy().as_ref()))
    {
        return Classification::Ignore;
    }

    // Allow key .git/ files that change on commit/pull/checkout, but skip
    // noisy internals that cause feedback loops with git2.
    if changed_path.components().any(|c| c.as_os_str() == ".git") {
        let name = changed_path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let path_str = changed_path.to_string_lossy();
        let is_meaningful = name == "HEAD"
            || name == "index"
            || name == "MERGE_HEAD"
            || name == "REBASE_HEAD"
            || name == "COMMIT_EDITMSG"
            || name == "packed-refs"
            || path_str.contains(".git/refs/");
        if !is_meaningful {
            return Classification::Ignore;
        }
    }

    // Inside a known repo? Route the change to it.
    for repo_path in repo_paths {
        if changed_path.starts_with(repo_path) {
            return Classification::Repo(repo_path.clone());
        }
    }

    // Otherwise, treat as a root-level event only when the change is a direct
    // child of a configured root. This filters out the recursive event noise
    // macOS FSEvents delivers regardless of the requested watch depth and
    // limits the trigger to the depth-1 case where `discover_repos` will
    // actually find the new repo.
    for root in root_dirs {
        if let Some(parent) = changed_path.parent()
            && parent == root.as_path()
        {
            return Classification::RootDir;
        }
    }

    Classification::Ignore
}

impl RepoWatcher {
    pub fn new(
        repo_paths: &[PathBuf],
        root_dirs: &[PathBuf],
        debounce_ms: u64,
        event_tx: UnboundedSender<Event>,
        watch_exclude_dirs: &[String],
    ) -> color_eyre::Result<Self> {
        let owned_repo_paths: Vec<PathBuf> = repo_paths.to_vec();
        let owned_root_dirs: Vec<PathBuf> = root_dirs.to_vec();

        // Bridge channel: notify callback (OS thread) -> tokio task
        let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathBuf>>();

        // Spawn tokio task to route changed paths to repo paths or root dirs.
        let repos_for_routing = owned_repo_paths.clone();
        let roots_for_routing = owned_root_dirs.clone();
        let exclude_set: HashSet<String> = watch_exclude_dirs.iter().cloned().collect();
        tokio::spawn(async move {
            while let Some(changed_paths) = bridge_rx.recv().await {
                let mut affected_repos: HashSet<PathBuf> = HashSet::new();
                let mut roots_changed = false;

                for changed_path in &changed_paths {
                    match classify(
                        changed_path,
                        &repos_for_routing,
                        &roots_for_routing,
                        &exclude_set,
                    ) {
                        Classification::Repo(repo) => {
                            affected_repos.insert(repo);
                        }
                        Classification::RootDir => {
                            roots_changed = true;
                        }
                        Classification::Ignore => {}
                    }
                }

                for path in affected_repos {
                    let _ = event_tx.send(Event::RepoChanged(path));
                }
                if roots_changed {
                    let _ = event_tx.send(Event::ReposRootChanged);
                }
            }
        });

        let config = Config::default().with_poll_interval(Duration::from_secs(2));

        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            Duration::from_millis(debounce_ms),
            None,
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    let paths: Vec<PathBuf> =
                        events.into_iter().flat_map(|e| e.event.paths).collect();
                    if !paths.is_empty() {
                        let _ = bridge_tx.send(paths);
                    }
                }
            },
            NoCache,
            config,
        )?;

        // Watch each repo root recursively (existing behavior).
        for path in &owned_repo_paths {
            if path.exists()
                && let Err(e) = debouncer.watch(path, RecursiveMode::Recursive)
            {
                tracing::warn!("Failed to watch repo {}: {}", path.display(), e);
            }
        }

        // Watch each configured root non-recursively so we notice top-level
        // children appearing or disappearing (new clones, deleted repos).
        // FSEvents on macOS may still deliver events for deeper paths; the
        // routing classifier above filters those out.
        for root in &owned_root_dirs {
            if root.exists()
                && let Err(e) = debouncer.watch(root, RecursiveMode::NonRecursive)
            {
                tracing::warn!("Failed to watch root dir {}: {}", root.display(), e);
            }
        }

        Ok(Self {
            _debouncer: debouncer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    fn exclude(set: &[&str]) -> HashSet<String> {
        set.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classify_routes_inside_known_repo() {
        let repos = vec![s("/Code/repo-a")];
        let roots = vec![s("/Code")];
        let r = classify(
            &s("/Code/repo-a/src/main.rs"),
            &repos,
            &roots,
            &exclude(&[]),
        );
        assert_eq!(r, Classification::Repo(s("/Code/repo-a")));
    }

    #[test]
    fn classify_emits_root_change_for_direct_child_of_root() {
        let repos: Vec<PathBuf> = vec![];
        let roots = vec![s("/Code")];
        let r = classify(&s("/Code/new-repo"), &repos, &roots, &exclude(&[]));
        assert_eq!(r, Classification::RootDir);
    }

    #[test]
    fn classify_ignores_deeply_nested_path_outside_known_repos() {
        // This is the FSEvents-on-macOS case: NonRecursive root watching may
        // still deliver events for deeper paths. We want them dropped.
        let repos: Vec<PathBuf> = vec![];
        let roots = vec![s("/Code")];
        let r = classify(
            &s("/Code/unknown-dir/deep/file.txt"),
            &repos,
            &roots,
            &exclude(&[]),
        );
        assert_eq!(r, Classification::Ignore);
    }

    #[test]
    fn classify_ignores_root_dir_itself() {
        let repos: Vec<PathBuf> = vec![];
        let roots = vec![s("/Code")];
        let r = classify(&s("/Code"), &repos, &roots, &exclude(&[]));
        // /Code has no parent equal to a root → Ignore.
        assert_eq!(r, Classification::Ignore);
    }

    #[test]
    fn classify_ignores_excluded_components() {
        let repos = vec![s("/Code/repo-a")];
        let roots = vec![s("/Code")];
        let r = classify(
            &s("/Code/repo-a/node_modules/foo.js"),
            &repos,
            &roots,
            &exclude(&["node_modules"]),
        );
        assert_eq!(r, Classification::Ignore);
    }

    #[test]
    fn classify_keeps_meaningful_git_files() {
        let repos = vec![s("/Code/repo-a")];
        let roots = vec![s("/Code")];
        let r = classify(&s("/Code/repo-a/.git/HEAD"), &repos, &roots, &exclude(&[]));
        assert_eq!(r, Classification::Repo(s("/Code/repo-a")));
    }

    #[test]
    fn classify_drops_git_internals() {
        let repos = vec![s("/Code/repo-a")];
        let roots = vec![s("/Code")];
        let r = classify(
            &s("/Code/repo-a/.git/objects/ab/cdef"),
            &repos,
            &roots,
            &exclude(&[]),
        );
        assert_eq!(r, Classification::Ignore);
    }

    #[test]
    fn classify_prefers_repo_match_over_root_match() {
        // A path that's both inside a known repo and a direct child of a
        // root should route to the repo, not trigger a rescan.
        let repos = vec![s("/Code/repo-a")];
        let roots = vec![s("/Code")];
        let r = classify(&s("/Code/repo-a"), &repos, &roots, &exclude(&[]));
        assert_eq!(r, Classification::Repo(s("/Code/repo-a")));
    }
}
