use crate::config::Config;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Cheap check that a `.git` entry looks real. A bare `mkdir .git` (which
/// can happen by accident, e.g. an aborted clone) is enough to fool a
/// `.git.exists()` test but `Repository::open` then fails on every status
/// query, so we treat such paths as not-a-repo at discovery time. Anything
/// produced by `git init` will have a `HEAD` file; that is what we look for.
///
/// Three layouts are accepted:
/// - A standard checkout, where `.git` is a directory containing `HEAD`.
/// - A *symlink* to such a git directory. This is exactly what Google's
///   `repo` tool writes for every project working copy (`.git ->`
///   `.repo/projects/<name>.git`), and `Path::join("HEAD").is_file()`
///   resolves the symlink transparently.
/// - A submodule or linked worktree, where `.git` is a *file* of the form
///   `gitdir: <path>` pointing at the real git directory (e.g.
///   `<superproject>/.git/modules/<name>`). Without this branch, pinning a
///   submodule would pass `AddRepo`'s `.git.exists()` check but then vanish on
///   the next FS-driven rescan, because discovery couldn't find its `HEAD`.
///
/// Only the first layout is reachable from the root-dir tree walk. The other
/// two describe paths the user (or `repo sync`) named explicitly — a pinned
/// submodule, a pinned worktree, a `.repo/project.list` entry — and the walk
/// deliberately ignores them; see the `is_dir` check in [`discover_repos`].
fn is_real_git_dir(dot_git: &Path) -> bool {
    if dot_git.join("HEAD").is_file() {
        return true;
    }
    if dot_git.is_file()
        && let Some(gitdir) = read_gitdir_pointer(dot_git)
    {
        return gitdir.join("HEAD").is_file();
    }
    false
}

/// Whether a candidate repo path matches an `excluded_repos` pattern.
/// Patterns match the repo's directory name or any path component.
fn is_excluded(repo_path: &Path, config: &Config) -> bool {
    let repo_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let path_str = repo_path.to_string_lossy();
    config
        .excluded_repos
        .iter()
        .any(|pattern| repo_name == *pattern || path_str.contains(pattern))
}

/// Google `repo` (git-repo) managed workspace discovery.
///
/// In such a workspace every project's working copy carries a `.git` that is
/// a *symlink* (or `gitdir:` file) pointing into `.repo/projects/`, so the
/// tree walk never sees them as directories. The authoritative list of
/// managed projects is `.repo/project.list` — one relative path per line —
/// which we enumerate instead. This is exact (matches what `repo sync`
/// manages), scales to hundreds of projects, and automatically picks up
/// projects added or removed by later `repo sync` runs because the file is
/// re-read on every discovery pass. Workspaces not managed by `repo` (no
/// `.repo/project.list`) are left untouched.
fn discover_repo_workspace_projects(
    root: &Path,
    config: &Config,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let contents = match std::fs::read_to_string(root.join(".repo").join("project.list")) {
        Ok(c) => c,
        Err(_) => return, // not a repo-managed workspace (or not synced yet)
    };
    // Containment is decided in canonical form. `project.list` is generated
    // from a manifest the workspace fetches from a remote, so its entries are
    // untrusted: `root.join("../../etc/x")` keeps the `..` components
    // verbatim, so a lexical `starts_with(root)` says yes while the path
    // actually resolves outside the workspace.
    let Ok(canonical_root) = root.canonicalize() else {
        return;
    };
    for line in contents.lines() {
        let rel = line.trim();
        if rel.is_empty() {
            continue;
        }
        // Canonicalizing also makes a symlinked root (e.g. `~/work` ->
        // `/real/path`) produce the same string as the tree walk's paths.
        // Without that, the same top-level repo would be added twice — once
        // by the walk, once here — and `seen` couldn't tell them apart.
        // A project whose working tree hasn't been synced yet doesn't
        // resolve at all, and is skipped along with the escapees.
        let Ok(canonical) = root.join(rel).canonicalize() else {
            continue;
        };
        if !canonical.starts_with(&canonical_root) {
            continue;
        }
        // The working copy's `.git` may be a symlink or a `gitdir:` file;
        // is_real_git_dir handles both and rejects empty/phantom dirs.
        if is_real_git_dir(&canonical.join(".git"))
            && !is_excluded(&canonical, config)
            && seen.insert(canonical.clone())
        {
            out.push(canonical);
        }
    }
}

/// Resolve a `gitdir: <path>` pointer file (used by submodules and linked
/// worktrees) to the git directory it references. Relative targets resolve
/// against the directory holding the pointer file.
fn read_gitdir_pointer(dot_git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(dot_git_file).ok()?;
    let target = Path::new(contents.trim().strip_prefix("gitdir:")?.trim());
    if target.is_absolute() {
        Some(target.to_path_buf())
    } else {
        Some(dot_git_file.parent()?.join(target))
    }
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
            // Don't descend into the repo tool's own metadata (`<root>/.repo`).
            // Its gitdirs under `.repo/projects/` are not `.git` entries, but
            // `.repo/manifests/.git` is a symlink that would otherwise match.
            .filter_entry(|e| e.file_name() != ".repo")
            .filter_map(|e| e.ok())
        {
            // The walk only ever promotes a real `.git` *directory*. A `.git`
            // symlink or `gitdir:` pointer file belongs to something that
            // already has an owner: a linked worktree (which gitpane creates
            // as a sibling of its repo, so it would appear both here and
            // nested under its parent), a submodule (which the parent repo
            // renders inline), or a repo-workspace project. Those reach the
            // list through `pinned_repos` or `.repo/project.list` instead.
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

                if !is_excluded(&repo_path, config) && seen.insert(repo_path.clone()) {
                    repos.push(repo_path);
                }
            }
        }

        // Google `repo` (git-repo) managed workspace: enumerate
        // `.repo/project.list` for the projects the tree walk cannot see.
        discover_repo_workspace_projects(root, config, &mut seen, &mut repos);
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

    /// A git submodule (or linked worktree): `.git` is a *file* containing a
    /// `gitdir:` pointer to the real git directory, not a directory of its own.
    /// `make_repo` can't model this because it always creates a `.git` dir.
    fn make_submodule(
        parent: &std::path::Path,
        name: &str,
        super_git: &std::path::Path,
    ) -> PathBuf {
        let repo_dir = parent.join(name);
        fs::create_dir_all(&repo_dir).unwrap();
        // The real git dir lives under the superproject, e.g.
        // `<super>/.git/modules/<name>`, and carries the HEAD file.
        let module_git = super_git.join("modules").join(name);
        fs::create_dir_all(&module_git).unwrap();
        fs::write(module_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // The working tree's `.git` is a pointer file with an absolute target.
        fs::write(
            repo_dir.join(".git"),
            format!("gitdir: {}\n", module_git.display()),
        )
        .unwrap();
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

    /// Regression: pinning a submodule (whose `.git` is a pointer file, not a
    /// directory) must survive discovery. Previously `is_real_git_dir` only
    /// accepted a `.git` directory with HEAD, so a pinned submodule passed
    /// `AddRepo` but was pruned on the next FS-driven rescan, vanishing from
    /// the list almost immediately after being added.
    #[test]
    fn test_pinned_submodule_is_discovered() {
        let tmp = TempDir::new().unwrap();
        let super_git = tmp.path().join("superproject").join(".git");
        fs::create_dir_all(&super_git).unwrap();
        let submodule = make_submodule(tmp.path(), "vendored-lib", &super_git);

        let config = Config {
            root_dirs: vec![],
            pinned_repos: vec![submodule.clone()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("vendored-lib"));
    }

    /// A relative `gitdir:` pointer (the form real `git submodule` writes,
    /// e.g. `gitdir: ../../.git/modules/<name>`) must resolve against the
    /// working tree, not the process CWD.
    #[test]
    fn test_pinned_submodule_relative_gitdir_is_discovered() {
        let tmp = TempDir::new().unwrap();
        // Layout: tmp/super/.git/modules/sub  and  tmp/super/deps/sub
        let super_dir = tmp.path().join("super");
        let module_git = super_dir.join(".git").join("modules").join("sub");
        fs::create_dir_all(&module_git).unwrap();
        fs::write(module_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let work = super_dir.join("deps").join("sub");
        fs::create_dir_all(&work).unwrap();
        // Relative pointer from tmp/super/deps/sub back to the module git dir.
        fs::write(work.join(".git"), "gitdir: ../../.git/modules/sub\n").unwrap();

        let config = Config {
            root_dirs: vec![],
            pinned_repos: vec![work.clone()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("sub"));
    }

    /// Regression: a linked worktree that sits *beside* its repo must not
    /// become a top-level row of its own. `worktree_path` puts new worktrees
    /// in the repo's parent directory by default, i.e. directly under the
    /// configured root, and their `.git` is a `gitdir:` pointer file. If the
    /// walk promoted pointer files, every worktree gitpane creates would show
    /// up twice: once as its own repo and once nested under its parent.
    #[test]
    fn test_sibling_worktree_is_not_a_top_level_repo() {
        let tmp = TempDir::new().unwrap();
        let repo = make_repo(tmp.path(), "proj");
        // What `git worktree add ../proj-feature` leaves on disk: an admin
        // dir under the repo, and a pointer file in the new working tree.
        let admin = repo.join(".git").join("worktrees").join("feature");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        let worktree = tmp.path().join("proj-feature");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();

        let config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };

        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(repos[0].ends_with("proj"));
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

    /// Build a Google `repo`-style workspace fixture under `root`:
    /// - `.repo/project.list` lists `projects` (one relative path per line),
    /// - each project's gitdir lives at `.repo/projects/<name>.git` with a HEAD,
    /// - the working copy at `<root>/<name>` gets its `.git` linked by `link`
    ///   (a symlink on unix — the layout real `repo sync` writes — or a
    ///   `gitdir:` pointer file, the portable equivalent).
    fn make_repo_workspace(root: &Path, projects: &[&str], link: impl Fn(&Path, &Path)) {
        fs::create_dir_all(root.join(".repo/projects")).unwrap();
        for name in projects {
            let work = root.join(name);
            fs::create_dir_all(&work).unwrap();
            let gitdir = root.join(".repo/projects").join(format!("{name}.git"));
            fs::create_dir_all(&gitdir).unwrap();
            fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            link(&work, &gitdir);
        }
        fs::write(
            root.join(".repo").join("project.list"),
            format!("{}\n", projects.join("\n")),
        )
        .unwrap();
    }

    /// Whether discovery returned a repo whose path ends with `suffix`.
    ///
    /// Discovery canonicalizes every path it emits, and on macOS the temp dir
    /// canonicalizes from `/var/...` to `/private/var/...`, so comparing
    /// against a `TempDir`-derived `PathBuf` never matches. Matching on the
    /// trailing components is what the rest of this module does.
    fn found(repos: &[PathBuf], suffix: &str) -> bool {
        repos.iter().any(|r| r.ends_with(suffix))
    }

    /// A Google `repo` workspace where every project's `.git` is a symlink to
    /// `.repo/projects/<name>.git` — the layout `repo sync` actually writes on
    /// unix. Before repo-aware discovery, all of these were invisible because
    /// the walk's `is_dir()` check rejected symlinks.
    #[cfg(unix)]
    #[test]
    fn test_repo_workspace_symlink_projects_are_discovered() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let projects = ["kernel-6.12", "hbre/libmm", "app/qualitytest"];
        make_repo_workspace(root, &projects, |work, gitdir| {
            std::os::unix::fs::symlink(gitdir, work.join(".git")).unwrap()
        });

        let config = Config {
            root_dirs: vec![root.to_path_buf()],
            scan_depth: 1, // the walk sees nothing; project.list does the work
            ..Config::default()
        };
        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 3, "got {repos:?}");
        for name in projects {
            assert!(found(&repos, name), "missing {name}, got {repos:?}");
        }
    }

    /// Same workspace layout but with `gitdir:` pointer files instead of
    /// symlinks (portable; some repo versions/OSes write these).
    #[test]
    fn test_repo_workspace_gitdir_file_projects_are_discovered() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let projects = ["kernel-6.12", "hbre/libmm"];
        make_repo_workspace(root, &projects, |work, gitdir| {
            fs::write(work.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap()
        });

        let config = Config {
            root_dirs: vec![root.to_path_buf()],
            scan_depth: 1,
            ..Config::default()
        };
        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 2, "got {repos:?}");
        for name in projects {
            assert!(found(&repos, name), "missing {name}, got {repos:?}");
        }
    }

    /// A project listed in `.repo/project.list` whose working tree hasn't been
    /// synced yet must be skipped, not crash discovery.
    #[test]
    fn test_repo_workspace_skips_missing_worktrees() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_repo_workspace(root, &["synced"], |work, gitdir| {
            fs::write(work.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap()
        });
        // A second project that exists only in project.list, not on disk.
        fs::write(
            root.join(".repo").join("project.list"),
            "synced\nnot-synced-yet\n",
        )
        .unwrap();

        let config = Config {
            root_dirs: vec![root.to_path_buf()],
            scan_depth: 1,
            ..Config::default()
        };
        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(found(&repos, "synced"), "got {repos:?}");
    }

    /// `.repo/project.list` is generated from a manifest the workspace fetches
    /// from a remote, so an entry can name a path outside the root. Rejecting
    /// it has to happen after canonicalization: `<root>/../outside` still has
    /// `<root>` as a lexical prefix, so the pre-canonical check waves it
    /// through and discovery adopts a repo the user never configured.
    #[test]
    fn test_repo_workspace_rejects_paths_escaping_the_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ws");
        fs::create_dir_all(&root).unwrap();
        // A real repo sitting just outside the workspace root.
        make_repo(tmp.path(), "outside");
        make_repo_workspace(&root, &["inside"], |work, gitdir| {
            fs::write(work.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap()
        });
        fs::write(
            root.join(".repo").join("project.list"),
            "inside\n../outside\n",
        )
        .unwrap();

        let config = Config {
            root_dirs: vec![root.clone()],
            scan_depth: 1,
            ..Config::default()
        };
        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 1, "got {repos:?}");
        assert!(found(&repos, "inside"), "got {repos:?}");
        assert!(!found(&repos, "outside"), "got {repos:?}");
    }

    /// Native `.git` directories outside `.repo` must still be found alongside
    /// repo-managed projects, and the repo tool's own `.repo/manifests/.git`
    /// symlink must NOT be picked up as a project.
    #[cfg(unix)]
    #[test]
    fn test_repo_workspace_mixes_native_and_managed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_repo(root, "standalone");
        make_repo_workspace(root, &["managed"], |work, gitdir| {
            std::os::unix::fs::symlink(gitdir, work.join(".git")).unwrap()
        });
        // Simulate repo's own tooling symlink inside `.repo/`.
        fs::create_dir_all(root.join(".repo/manifests")).unwrap();
        fs::create_dir_all(root.join(".repo/manifests-git")).unwrap();
        fs::write(
            root.join(".repo/manifests-git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            root.join(".repo/manifests-git"),
            root.join(".repo/manifests").join(".git"),
        )
        .unwrap();

        let config = Config {
            root_dirs: vec![root.to_path_buf()],
            scan_depth: 2,
            ..Config::default()
        };
        let repos = discover_repos(&config);
        assert_eq!(repos.len(), 2, "got {repos:?}");
        assert!(found(&repos, "standalone"), "got {repos:?}");
        assert!(found(&repos, "managed"), "got {repos:?}");
        assert!(!found(&repos, "manifests"), "got {repos:?}");
    }

    /// A repo workspace reached through a symlinked root (e.g. `~/work` ->
    /// `/real/path`) must not double-add the top-level projects: the tree
    /// walk canonicalizes its paths while project.list used to keep the
    /// symlinked form, so `seen` saw two different strings for the same repo.
    #[cfg(unix)]
    #[test]
    fn test_repo_workspace_symlink_root_has_no_duplicates() {
        let tmp = TempDir::new().unwrap();
        let real_root = tmp.path().join("real");
        fs::create_dir_all(real_root.join(".repo/projects")).unwrap();
        let mut worktrees = Vec::new();
        for name in ["build", "kernel-6.12", "hbre/libmm"] {
            let work = real_root.join(name);
            fs::create_dir_all(&work).unwrap();
            let gitdir = real_root.join(".repo/projects").join(format!("{name}.git"));
            fs::create_dir_all(&gitdir).unwrap();
            fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
            std::os::unix::fs::symlink(&gitdir, work.join(".git")).unwrap();
            worktrees.push(work);
        }
        fs::write(
            real_root.join(".repo").join("project.list"),
            "build\nkernel-6.12\nhbre/libmm\n",
        )
        .unwrap();

        // Access the workspace through a symlink alias, as `~/work` would be.
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real_root, &alias).unwrap();

        let config = Config {
            root_dirs: vec![alias],
            scan_depth: 2,
            ..Config::default()
        };
        let repos = discover_repos(&config);

        let unique: HashSet<&PathBuf> = repos.iter().collect();
        assert_eq!(repos.len(), unique.len(), "duplicates: {repos:?}");
        assert_eq!(repos.len(), 3, "got {repos:?}");
        // Every path is canonical (real) form, so list display is consistent.
        // The fixture root itself needs canonicalizing to compare: on macOS
        // the temp dir lives under `/var`, which resolves to `/private/var`.
        let canonical_root = real_root.canonicalize().unwrap();
        assert!(
            repos.iter().all(|p| p.starts_with(&canonical_root)),
            "non-canonical paths: {repos:?}"
        );
    }
}
