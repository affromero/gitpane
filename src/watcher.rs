use ignore::WalkBuilder;
use notify_debouncer_full::{
    DebounceEventResult, Debouncer, NoCache, new_debouncer_opt,
    notify::{Config, RecommendedWatcher, RecursiveMode},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
/// drag in restricted system paths like `/tmp/systemd-private-*`. The repo's
/// own `.git` directory is pruned here and re-watched selectively by
/// `watch_git_metadata`, so we never install watches across `.git/objects`.
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
        && (name == ".git" || exclude_set.contains(name))
    {
        return false;
    }
    true
}

/// Enumerate the working-tree directories that should receive a watch. The walk
/// is `.gitignore`-aware (via the `ignore` crate, honoring `.gitignore`,
/// `.git/info/exclude`, and the global gitignore), skips symlinks and any
/// directory whose name is in `exclude_set`, and prunes the repo's own `.git`
/// directory (re-watched separately by `watch_git_metadata`).
///
/// Respecting `.gitignore` is the whole point: without it, the startup walk
/// stats every ignored file and installs a watch per directory across huge
/// vendored / data / virtualenv trees (an ML checkout can hold >1M ignored
/// files), which blocks startup. Changes inside ignored directories never
/// affect `git status`, so watching them is pure waste.
fn watch_dirs(root: &Path, exclude_set: &HashSet<String>) -> Vec<PathBuf> {
    // `ignore`'s filter_entry closure is `Send + Sync + 'static`, so it cannot
    // borrow `exclude_set`; share an owned copy in.
    let predicate_excludes = Arc::new(exclude_set.clone());
    WalkBuilder::new(root)
        .hidden(false) // keep .github / .vscode etc.; .git is pruned by the predicate
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false)
        .filter_entry(move |e| {
            should_keep_walk_entry(
                e.path_is_symlink(),
                e.depth(),
                e.file_name().to_str(),
                &predicate_excludes,
            )
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_dir()))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Install a non-recursive notify watch on each gitignore-aware working-tree
/// directory, then re-add the `.git` metadata watches the change classifier
/// depends on. We do the walk ourselves (rather than asking notify for
/// `RecursiveMode::Recursive`) so we never descend into symlinks that point at
/// restricted system paths, and so ignored / build dirs never hit inotify.
fn install_filtered_watches(
    debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
    root: &Path,
    exclude_set: &HashSet<String>,
    watch_worktree_dirs: bool,
) {
    if watch_worktree_dirs {
        let mut watched_root = false;
        for dir in watch_dirs(root, exclude_set) {
            let is_root = dir.as_path() == root;
            match debouncer.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    if is_root {
                        watched_root = true;
                    }
                }
                Err(e) => {
                    if is_root {
                        tracing::warn!("Failed to watch repo {}: {}", dir.display(), e);
                    } else {
                        tracing::debug!("skip watch on {}: {}", dir.display(), e);
                    }
                }
            }
        }
        if !watched_root {
            tracing::debug!("no usable watch installed for repo {}", root.display());
        }
    } else {
        match debouncer.watch(root, RecursiveMode::NonRecursive) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("Failed to watch repo {}: {}", root.display(), e);
            }
        }
    }

    // The working-tree walk prunes `.git`; re-add the watches we depend on.
    watch_git_metadata(debouncer, &root.join(".git"));
}

/// Re-install the small set of `.git` watches the change classifier depends on.
/// `classify` treats `.git/HEAD`, `index`, `packed-refs`, `MERGE_HEAD`,
/// `REBASE_HEAD`, `COMMIT_EDITMSG`, and anything under `.git/refs/` as
/// meaningful — that is how commits, checkouts, merges, and branch updates
/// trigger a refresh. We watch `.git` itself (its top-level files) plus every
/// directory under `.git/refs`, and deliberately skip `.git/objects` (huge and
/// never classified as meaningful). The scanner only admits repos whose `.git`
/// is a real directory, so a missing/file `.git` here is a no-op.
fn watch_git_metadata(debouncer: &mut Debouncer<RecommendedWatcher, NoCache>, git_dir: &Path) {
    if !git_dir.is_dir() {
        return;
    }
    if let Err(e) = debouncer.watch(git_dir, RecursiveMode::NonRecursive) {
        tracing::debug!("skip watch on {}: {}", git_dir.display(), e);
    }
    let refs_dir = git_dir.join("refs");
    if !refs_dir.is_dir() {
        return;
    }
    for entry in WalkDir::new(&refs_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir()
            && let Err(e) = debouncer.watch(entry.path(), RecursiveMode::NonRecursive)
        {
            tracing::debug!("skip watch on {}: {}", entry.path().display(), e);
        }
    }
}

impl RepoWatcher {
    pub fn new(
        repo_paths: &[PathBuf],
        root_dirs: &[PathBuf],
        debounce_ms: u64,
        event_tx: UnboundedSender<Event>,
        watch_exclude_dirs: &[String],
        watch_worktree_dirs: bool,
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
            install_filtered_watches(&mut debouncer, path, &exclude_set, watch_worktree_dirs);
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
    fn walk_prunes_dot_git_below_root() {
        // .git is watched selectively by watch_git_metadata, never through the
        // working-tree walk (which would otherwise install watches across all
        // of .git/objects).
        let ex = exclude(&[]);
        assert!(!should_keep_walk_entry(false, 1, Some(".git"), &ex));
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

    #[test]
    fn watch_dirs_respects_gitignore() {
        // `ignore` only applies .gitignore inside a real git repo (require_git
        // defaults to true), so we actually `git init` rather than fake `.git`.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let initialized = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(initialized, "git must be available to run this test");

        std::fs::write(root.join(".gitignore"), "data/\n").unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("data").join("huge")).unwrap();

        let dirs = watch_dirs(root, &HashSet::new());
        let has_component = |needle: &str| {
            dirs.iter()
                .any(|d| d.components().any(|c| c.as_os_str() == needle))
        };

        assert!(
            dirs.iter().any(|d| d.as_path() == root),
            "repo root must be watched: {dirs:?}"
        );
        assert!(
            has_component("src"),
            "tracked dir src must be watched: {dirs:?}"
        );
        assert!(
            !has_component("data"),
            "gitignored data/ must NOT be watched: {dirs:?}"
        );
        assert!(
            !has_component(".git"),
            ".git must be excluded from the working-tree walk: {dirs:?}"
        );
    }
}
