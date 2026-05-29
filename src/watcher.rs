use notify_debouncer_full::{
    DebounceEventResult, Debouncer, NoCache, new_debouncer_opt,
    notify::{Config, RecommendedWatcher, RecursiveMode},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use walkdir::WalkDir;

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

/// Pure decision: should the walk descend into / install a watch for this
/// entry? `depth == 0` is the repo root itself — always kept (even if its
/// name matches an exclude, the user explicitly tracks it). Symlinks are
/// never followed: a Wine prefix's `dosdevices/z:` -> `/` would otherwise
/// drag in restricted system paths like `/tmp/systemd-private-*`.
fn should_keep_walk_entry(
    is_symlink: bool,
    depth: usize,
    name: Option<&str>,
    exclude_set: &HashSet<String>,
) -> bool {
    if is_symlink {
        return false;
    }
    if depth > 0
        && let Some(name) = name
        && exclude_set.contains(name)
    {
        return false;
    }
    true
}

/// Walk `root` with walkdir, skipping symlinks and any directory whose name
/// is in `exclude_set`, and install a non-recursive notify watch on each
/// remaining directory. We do the walk ourselves (rather than asking notify
/// for `RecursiveMode::Recursive`) so we never try to descend into symlinks
/// that point at restricted system paths, and so vendored / build dirs never
/// hit inotify at all.
fn install_filtered_watches(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    root: &Path,
    exclude_set: &HashSet<String>,
) {
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            should_keep_walk_entry(
                e.path_is_symlink(),
                e.depth(),
                e.file_name().to_str(),
                exclude_set,
            )
        });
    let mut watched_root = false;
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_dir() {
            continue;
        }
        match debouncer.watch(entry.path(), RecursiveMode::NonRecursive) {
            Ok(()) => {
                if entry.depth() == 0 {
                    watched_root = true;
                }
            }
            Err(e) => {
                if entry.depth() == 0 {
                    tracing::warn!("Failed to watch repo {}: {}", entry.path().display(), e);
                } else {
                    tracing::debug!("skip watch on {}: {}", entry.path().display(), e);
                }
            }
        }
    }
    if !watched_root {
        tracing::debug!("no usable watch installed for repo {}", root.display());
    }
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
        let exclude_set_for_routing = exclude_set.clone();
        tokio::spawn(async move {
            while let Some(changed_paths) = bridge_rx.recv().await {
                let mut affected_repos: HashSet<PathBuf> = HashSet::new();
                let mut roots_changed = false;

                for changed_path in &changed_paths {
                    match classify(
                        changed_path,
                        &repos_for_routing,
                        &roots_for_routing,
                        &exclude_set_for_routing,
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

        // Walk each repo ourselves and install a non-recursive watch per
        // directory, skipping symlinks and `watch_exclude_dirs` entries.
        // This stops notify from descending into things like a Wine prefix's
        // `dosdevices/z:` -> `/` link (which would attempt to watch
        // root-owned dirs like `/tmp/systemd-private-*` and emit permission
        // errors). It also keeps inotify off vendored / build dirs.
        for path in &owned_repo_paths {
            if !path.exists() {
                continue;
            }
            install_filtered_watches(&mut debouncer, path, &exclude_set);
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

    #[test]
    fn walk_keeps_root_even_if_name_is_excluded() {
        // The repo root is user-tracked; never drop it for matching an
        // exclude name like "target".
        let ex = exclude(&["target"]);
        assert!(should_keep_walk_entry(false, 0, Some("target"), &ex));
    }

    #[test]
    fn walk_skips_symlinks() {
        // Wine prefix's `dosdevices/z:` is a symlink to `/`; never descend.
        let ex = exclude(&[]);
        assert!(!should_keep_walk_entry(true, 3, Some("z:"), &ex));
    }

    #[test]
    fn walk_skips_excluded_dir_names_below_root() {
        let ex = exclude(&["node_modules", "target"]);
        assert!(!should_keep_walk_entry(false, 1, Some("node_modules"), &ex));
        assert!(!should_keep_walk_entry(false, 2, Some("target"), &ex));
    }

    #[test]
    fn walk_keeps_ordinary_subdir() {
        let ex = exclude(&["node_modules"]);
        assert!(should_keep_walk_entry(false, 1, Some("src"), &ex));
    }

    #[test]
    #[cfg(unix)]
    fn walk_excludes_symlink_subtree_on_real_fs() {
        // End-to-end: build a tree with a symlink whose target is a dir we
        // can't list (simulated with a regular dir we then mark unreadable
        // is unreliable, so we just check the symlink itself is skipped).
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir(root.join("dosdevices")).unwrap();
        // Point at the tempdir itself — a real-world wine prefix points to
        // `/`. We just need walkdir to encounter a symlinked dir.
        symlink(root, root.join("dosdevices").join("z:")).unwrap();

        let ex: HashSet<String> = HashSet::new();
        let mut visited: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                should_keep_walk_entry(e.path_is_symlink(), e.depth(), e.file_name().to_str(), &ex)
            })
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_path_buf())
            .collect();
        visited.sort();

        let symlink_path = root.join("dosdevices").join("z:");
        assert!(
            !visited.iter().any(|p| p == &symlink_path),
            "symlink should be skipped, got {visited:?}"
        );
        // And we definitely didn't recurse through it back into ourselves.
        assert!(
            !visited
                .iter()
                .any(|p| p.starts_with(&symlink_path) && p != &symlink_path),
            "no descendant of symlink should be visited, got {visited:?}"
        );
    }
}
