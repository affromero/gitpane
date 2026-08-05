use super::defaults::default_scan_depth;
use super::load::{LoadResolution, candidate_search_paths, default_write_path, resolve_load};
use super::terminal::{TERMINAL_GOTOS, goto_command_for_env};
use super::*;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
struct MockEnv {
    gitpane_config: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    project_config_dir: Option<PathBuf>,
    existing: HashSet<PathBuf>,
}

impl ConfigEnv for MockEnv {
    fn gitpane_config(&self) -> Option<PathBuf> {
        self.gitpane_config
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
    }

    fn xdg_config_home(&self) -> Option<PathBuf> {
        self.xdg_config_home
            .clone()
            .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.home_dir.clone()
    }

    fn project_config_dir(&self) -> Option<PathBuf> {
        self.project_config_dir.clone()
    }

    fn file_exists(&self, path: &Path) -> bool {
        self.existing.contains(path)
    }
}

fn path(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() || !value.starts_with('/') {
        path
    } else {
        std::env::current_dir()
            .unwrap()
            .join("mock-root")
            .join(value.trim_start_matches('/'))
    }
}

/// Regression: a sparse `~/.config/gitpane/config.toml` silently shadowed the
/// full config in the platform dir (macOS Application Support), making pinned
/// repos "vanish" after an update. The ignored file must be reported.
#[test]
fn test_shadowed_config_paths_reports_ignored_files() {
    let dot_config = path("/home/alice/.config/gitpane/config.toml");
    let native = path("/native/gitpane/config.toml");
    let env = MockEnv {
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        existing: HashSet::from([dot_config.clone(), native.clone()]),
        ..MockEnv::default()
    };

    let config = Config {
        loaded_path: Some(dot_config),
        ..Config::default()
    };
    assert_eq!(config.shadowed_config_paths(&env), vec![native.clone()]);

    // Single config file: nothing shadowed.
    let config = Config {
        loaded_path: Some(native.clone()),
        ..Config::default()
    };
    let env_single = MockEnv {
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        existing: HashSet::from([native]),
        ..MockEnv::default()
    };
    assert!(config.shadowed_config_paths(&env_single).is_empty());
}

#[test]
fn test_resolution_prefers_gitpane_config() {
    let env = MockEnv {
        gitpane_config: Some(path("/override/config.toml")),
        xdg_config_home: Some(path("/xdg")),
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        existing: HashSet::new(),
    };

    assert_eq!(
        resolve_load(&env),
        LoadResolution::EnvOverride(path("/override/config.toml"))
    );
}

#[test]
fn test_resolution_uses_xdg_config_home() {
    let env = MockEnv {
        xdg_config_home: Some(path("/xdg")),
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![
            path("/xdg/gitpane/config.toml"),
            path("/home/alice/.config/gitpane/config.toml"),
            path("/native/gitpane/config.toml"),
        ]
    );
}

#[test]
fn test_resolution_falls_back_to_dot_config() {
    let env = MockEnv {
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![
            path("/home/alice/.config/gitpane/config.toml"),
            path("/native/gitpane/config.toml"),
        ]
    );
}

#[test]
fn test_resolution_falls_back_to_native() {
    let env = MockEnv {
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![path("/native/gitpane/config.toml")]
    );
}

#[test]
fn test_resolution_returns_default_when_nothing_exists() {
    let env = MockEnv {
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.loaded_path, None);
    assert_eq!(config.write_target_override, None);
    assert_eq!(config.scan_depth, default_scan_depth());
}

#[test]
fn test_dedupe_collapses_xdg_dot_config_and_native_on_linux() {
    let env = MockEnv {
        xdg_config_home: Some(path("/home/alice/.config")),
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/home/alice/.config/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![path("/home/alice/.config/gitpane/config.toml")]
    );
}

#[test]
fn test_xdg_config_home_relative_is_ignored() {
    let env = MockEnv {
        xdg_config_home: Some(path("relative/xdg")),
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![
            path("/home/alice/.config/gitpane/config.toml"),
            path("/native/gitpane/config.toml"),
        ]
    );
}

#[test]
fn test_empty_gitpane_config_is_ignored() {
    let env = MockEnv {
        gitpane_config: Some(PathBuf::new()),
        home_dir: Some(path("/home/alice")),
        ..MockEnv::default()
    };

    assert_eq!(
        resolve_load(&env),
        LoadResolution::SearchOrder(vec![path("/home/alice/.config/gitpane/config.toml")])
    );
}

#[test]
fn test_empty_xdg_config_home_is_ignored() {
    let env = MockEnv {
        xdg_config_home: Some(PathBuf::new()),
        home_dir: Some(path("/home/alice")),
        ..MockEnv::default()
    };

    assert_eq!(
        candidate_search_paths(&env),
        vec![path("/home/alice/.config/gitpane/config.toml")]
    );
}

#[test]
fn test_default_write_path_prefers_xdg_when_set() {
    let env = MockEnv {
        xdg_config_home: Some(path("/xdg")),
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        default_write_path(&env),
        Some(path("/xdg/gitpane/config.toml"))
    );
}

#[test]
fn test_default_write_path_uses_dot_config_before_native() {
    let env = MockEnv {
        home_dir: Some(path("/home/alice")),
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        default_write_path(&env),
        Some(path("/home/alice/.config/gitpane/config.toml"))
    );
}

#[test]
fn test_default_write_path_uses_native_without_xdg_or_home() {
    let env = MockEnv {
        project_config_dir: Some(path("/native/gitpane")),
        ..MockEnv::default()
    };

    assert_eq!(
        default_write_path(&env),
        Some(path("/native/gitpane/config.toml"))
    );
}

#[test]
fn test_default_write_path_returns_none_when_no_path_is_available() {
    let env = MockEnv::default();
    assert_eq!(default_write_path(&env), None);
}

#[test]
fn test_gitpane_config_writes_to_env_path_even_when_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let override_path = tmp.path().join("missing").join("config.toml");
    let env = MockEnv {
        gitpane_config: Some(override_path.clone()),
        home_dir: Some(tmp.path().join("home")),
        project_config_dir: Some(tmp.path().join("native")),
        ..MockEnv::default()
    };

    let mut config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.loaded_path, None);
    assert_eq!(config.write_target_override, Some(override_path.clone()));

    config.pinned_repos.push(path("/tmp/pinned"));
    config.save_with_env(&env).unwrap();

    assert!(override_path.exists());
    let saved: Config = toml::from_str(&fs::read_to_string(&override_path).unwrap()).unwrap();
    assert_eq!(saved.pinned_repos, vec![path("/tmp/pinned")]);
}

#[test]
fn test_keybindings_parse_with_defaults() {
    let toml = r#"
        [[keybindings]]
        key = "b"
        command = "gh repo view --web"
        desc = "Open on github.com"

        [[keybindings]]
        key = "L"
        command = "lazygit"
        placement = "inline"
    "#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.keybindings.len(), 2);

    let b = &config.keybindings[0];
    assert_eq!(b.key, "b");
    assert_eq!(b.command, "gh repo view --web");
    // Placement is optional and defaults to `command`.
    assert_eq!(b.placement, "command");
    assert_eq!(b.desc.as_deref(), Some("Open on github.com"));

    let l = &config.keybindings[1];
    assert_eq!(l.placement, "inline");
    assert_eq!(l.desc, None);
}

#[test]
fn test_keybindings_default_empty() {
    // A config with no [[keybindings]] blocks yields an empty list, not an error.
    let config: Config = toml::from_str("scan_depth = 3").unwrap();
    assert!(config.keybindings.is_empty());
}

#[test]
fn test_key_matches_single_char_only() {
    use super::key_matches;
    // Exact single-char match fires; the wrong char and multi-char keys don't.
    assert!(key_matches("b", 'b'));
    assert!(key_matches("L", 'L'));
    assert!(!key_matches("b", 'c'));
    assert!(!key_matches("L", 'l')); // case-sensitive
    assert!(!key_matches("ctrl+b", 'b')); // multi-char key is inert
    assert!(!key_matches("", 'b'));
}

#[test]
fn test_gitpane_config_exclusive_does_not_fall_through() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lower_priority_path = tmp
        .path()
        .join("home")
        .join(".config")
        .join("gitpane")
        .join("config.toml");
    fs::create_dir_all(lower_priority_path.parent().unwrap()).unwrap();
    fs::write(&lower_priority_path, "scan_depth = 9\n").unwrap();

    let env = MockEnv {
        gitpane_config: Some(tmp.path().join("missing.toml")),
        home_dir: Some(tmp.path().join("home")),
        existing: HashSet::from([lower_priority_path]),
        ..MockEnv::default()
    };

    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.scan_depth, default_scan_depth());
    assert_eq!(config.loaded_path, None);
}

#[test]
fn test_save_writes_back_to_loaded_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("native").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, "scan_depth = 3\npinned_repos = []\n").unwrap();

    let env = MockEnv {
        project_config_dir: Some(config_path.parent().unwrap().to_path_buf()),
        existing: HashSet::from([config_path.clone()]),
        ..MockEnv::default()
    };

    let mut config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.scan_depth, 3);
    assert_eq!(config.loaded_path, Some(config_path.clone()));

    config.pinned_repos.push(path("/tmp/test-repo"));
    config.save_with_env(&env).unwrap();

    let saved: Config = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(saved.pinned_repos, vec![path("/tmp/test-repo")]);
}

#[test]
fn test_save_writes_to_xdg_default_when_not_loaded() {
    let tmp = tempfile::TempDir::new().unwrap();
    let xdg_home = tmp.path().join("xdg");
    let expected_path = xdg_home.join("gitpane").join("config.toml");
    let env = MockEnv {
        xdg_config_home: Some(xdg_home),
        home_dir: Some(tmp.path().join("home")),
        project_config_dir: Some(tmp.path().join("native")),
        ..MockEnv::default()
    };

    let mut config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.loaded_path, None);

    config.pinned_repos.push(path("/tmp/xdg-repo"));
    config.save_with_env(&env).unwrap();

    assert!(expected_path.exists());
    let saved: Config = toml::from_str(&fs::read_to_string(&expected_path).unwrap()).unwrap();
    assert_eq!(saved.pinned_repos, vec![path("/tmp/xdg-repo")]);
}

#[test]
fn test_default_config_has_code_root() {
    let config = Config::default();
    assert!(!config.root_dirs.is_empty());
    let first = &config.root_dirs[0];
    assert!(first.ends_with("Code"));
}

#[test]
fn test_cli_root_overrides_config() {
    let mut config = Config::default();
    config.override_root(PathBuf::from("/tmp/my-repos"));
    assert_eq!(config.root_dirs, vec![PathBuf::from("/tmp/my-repos")]);
}

#[test]
fn test_save_and_reload_roundtrip() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let mut config = Config::default();
    config.pinned_repos.push(PathBuf::from("/tmp/test-repo"));

    // Write directly to temp path
    let contents = toml::to_string_pretty(&config).unwrap();
    std::fs::write(&path, &contents).unwrap();

    let loaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.pinned_repos, vec![PathBuf::from("/tmp/test-repo")]);
}

#[test]
fn test_add_pinned_repo_deduplication() {
    let mut config = Config::default();
    config.add_pinned_repo(PathBuf::from("/tmp/repo-a"));
    config.add_pinned_repo(PathBuf::from("/tmp/repo-a"));
    config.add_pinned_repo(PathBuf::from("/tmp/repo-b"));
    assert_eq!(config.pinned_repos.len(), 2);
}

#[test]
fn test_branch_filter_parse_local() {
    let toml_str = r#"
        [graph]
        branches = "local"
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.graph.branches, BranchFilter::Local);
}

#[test]
fn test_graph_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.graph.branches, BranchFilter::All);
    assert_eq!(config.graph.label_max_len, 24);
}

#[test]
fn test_graph_config_roundtrip() {
    let mut config = Config::default();
    config.graph.branches = BranchFilter::Remote;
    config.graph.label_max_len = 16;

    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(loaded.graph.branches, BranchFilter::Remote);
    assert_eq!(loaded.graph.label_max_len, 16);
}

#[test]
fn test_show_stats_defaults_true() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.graph.show_stats);
}

#[test]
fn test_show_stats_roundtrip() {
    let mut config = Config::default();
    config.graph.show_stats = false;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert!(!loaded.graph.show_stats);
}

#[test]
fn test_check_for_updates_defaults_true() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.ui.check_for_updates);
    assert_eq!(config.ui.update_position, UpdatePosition::TopRight);
}

#[test]
fn test_update_position_parse() {
    let toml_str = r#"
        [ui]
        check_for_updates = false
        update_position = "top-left"
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(!config.ui.check_for_updates);
    assert_eq!(config.ui.update_position, UpdatePosition::TopLeft);
}

#[test]
fn test_update_config_roundtrip() {
    let mut config = Config::default();
    config.ui.check_for_updates = false;
    config.ui.update_position = UpdatePosition::TopLeft;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert!(!loaded.ui.check_for_updates);
    assert_eq!(loaded.ui.update_position, UpdatePosition::TopLeft);
}

#[test]
fn test_submodule_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert!(!config.submodules.ignore_dirty);
    assert!(config.submodules.warn_unpushed);
}

#[test]
fn test_submodule_config_roundtrip() {
    let mut config = Config::default();
    config.submodules.ignore_dirty = true;
    config.submodules.warn_unpushed = false;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert!(loaded.submodules.ignore_dirty);
    assert!(!loaded.submodules.warn_unpushed);
}

#[test]
fn test_submodule_config_parse() {
    let toml_str = r#"
        [submodules]
        ignore_dirty = true
        warn_unpushed = false
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.submodules.ignore_dirty);
    assert!(!config.submodules.warn_unpushed);
}

#[test]
fn test_github_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.github.enabled);
    assert!(Config::default().github.enabled);
}

#[test]
fn test_github_config_roundtrip() {
    let mut config = Config::default();
    config.github.enabled = false;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert!(!loaded.github.enabled);
}

#[test]
fn test_github_config_parse() {
    let toml_str = r#"
        [github]
        enabled = false
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(!config.github.enabled);
}

#[test]
fn test_open_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.open.command.is_none());
    assert_eq!(config.open.placement, "command");
    // Config::default() is a live runtime fallback, so its placement must
    // match the serde default (not "" from a derived Default).
    assert_eq!(Config::default().open.placement, "command");
}

#[test]
fn test_open_config_roundtrip() {
    let mut config = Config::default();
    config.open.command = Some("cursor {path}".into());
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(loaded.open.command.as_deref(), Some("cursor {path}"));
}

#[test]
fn test_open_config_parse() {
    let toml_str = r#"
        [open]
        command = "tmux new-window -c {path}"
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(
        config.open.command.as_deref(),
        Some("tmux new-window -c {path}")
    );
}

#[test]
fn test_review_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.review.command.is_none());
    assert!(config.review.base.is_none());
    assert_eq!(config.review.placement, "new-window");
    assert_eq!(Config::default().review.placement, "new-window");
}

#[test]
fn test_review_config_roundtrip() {
    let mut config = Config::default();
    config.review.command = Some("git diff {base}...HEAD | delta".into());
    config.review.base = Some("origin/main".into());
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(
        loaded.review.command.as_deref(),
        Some("git diff {base}...HEAD | delta")
    );
    assert_eq!(loaded.review.base.as_deref(), Some("origin/main"));
}

#[test]
fn test_review_config_parse() {
    let toml_str = r#"
        [review]
        command = "difft"
        base = "develop"
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.review.command.as_deref(), Some("difft"));
    assert_eq!(config.review.base.as_deref(), Some("develop"));
}

#[test]
fn test_worktree_config_roundtrip() {
    let mut config = Config::default();
    assert!(config.worktree.dir.is_none());
    config.worktree.dir = Some(PathBuf::from("/wt"));
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(loaded.worktree.dir, Some(PathBuf::from("/wt")));
}

#[test]
fn test_worktree_path_sibling_when_dir_unset() {
    let cfg = WorktreeConfig::default();
    assert_eq!(
        worktree_path(&cfg, Path::new("/home/me/code/app"), "feat/x"),
        PathBuf::from("/home/me/code/app-feat-x")
    );
}

#[test]
fn test_worktree_path_under_configured_dir() {
    let cfg = WorktreeConfig {
        dir: Some(PathBuf::from("/wt")),
    };
    assert_eq!(
        worktree_path(&cfg, Path::new("/home/me/code/app"), "bugfix"),
        PathBuf::from("/wt/app-bugfix")
    );
}

#[test]
fn test_goto_command_table() {
    let cmd = |term_var: &str| goto_command_for_env(|v| v == term_var);
    assert!(cmd("WEZTERM_PANE").contains("wezterm cli spawn"));
    assert!(cmd("KITTY_WINDOW_ID").contains("--type=tab"));
    assert!(
        cmd("GHOSTTY_RESOURCES_DIR")
            .to_lowercase()
            .contains("ghostty")
    );
    assert!(cmd("KONSOLE_VERSION").contains("konsole --new-tab"));
    assert!(cmd("ALACRITTY_SOCKET").contains("alacritty msg create-window"));
    assert!(cmd("GNOME_TERMINAL_SCREEN").contains("gnome-terminal --tab"));
    // Unknown terminal: empty (never an in-place switch fallback).
    assert!(goto_command_for_env(|_| false).is_empty());
    // Every table entry carries the {session} token, opens a new view, and
    // is classifiable so the menu can show a "(new tab/window)" label.
    for t in TERMINAL_GOTOS {
        assert!(
            t.command.contains("{session}"),
            "{} missing token",
            t.command
        );
        assert!(
            !t.command.contains("switch-client"),
            "{} must not switch in place",
            t.command
        );
        assert!(
            crate::session::launcher::goto_placement(t.command).is_some(),
            "{} has no placement label",
            t.command
        );
    }
}

#[test]
fn test_goto_config_parse_overrides_default() {
    let toml_str = r#"
        [goto]
        command = "wezterm cli spawn -- tmux attach -t {session}"
    "#;
    let parsed: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(
        parsed.goto.command,
        "wezterm cli spawn -- tmux attach -t {session}"
    );
}

#[test]
fn test_worktree_dir_tilde_expanded() {
    if let Some(home) = dirs::home_dir() {
        let mut config = Config::default();
        config.worktree.dir = Some(PathBuf::from("~/worktrees"));
        config.expand_tildes();
        assert_eq!(config.worktree.dir, Some(home.join("worktrees")));
    }
}

#[test]
fn test_warn_unpushed_defaults_true_when_only_ignore_dirty_set() {
    let toml_str = r#"
        [submodules]
        ignore_dirty = true
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.submodules.ignore_dirty);
    assert!(config.submodules.warn_unpushed);
}

#[test]
fn test_max_concurrent_polls_default() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.watch.max_concurrent_polls, 4);
}

#[test]
fn test_refresh_cooldown_default() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.watch.refresh_cooldown_ms, 5000);
}

#[test]
fn test_watch_worktree_dirs_default() {
    let config: Config = toml::from_str("").unwrap();
    assert!(!config.watch.watch_worktree_dirs);
}

#[test]
fn test_watch_exclude_dirs_default() {
    let config: Config = toml::from_str("").unwrap();
    assert!(
        config
            .watch
            .watch_exclude_dirs
            .contains(&"node_modules".to_string())
    );
    assert!(
        config
            .watch
            .watch_exclude_dirs
            .contains(&"target".to_string())
    );
    assert!(
        config
            .watch
            .watch_exclude_dirs
            .contains(&".next".to_string())
    );
}

#[test]
fn test_theme_defaults_to_default_name() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.theme_name, "default");
}

#[test]
fn test_theme_field_in_toml_populates_theme_name() {
    let config: Config = toml::from_str("theme = \"muted\"").unwrap();
    assert_eq!(config.theme_name, "muted");
}

#[test]
fn test_load_with_env_resolves_default_theme() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_dir = tmp.path().join("gitpane");
    fs::create_dir_all(&config_dir).unwrap();
    let cfg_path = config_dir.join(CONFIG_FILE);
    fs::write(&cfg_path, "").unwrap();

    let env = MockEnv {
        xdg_config_home: Some(tmp.path().to_path_buf()),
        existing: HashSet::from([cfg_path.clone()]),
        ..Default::default()
    };
    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(
        config.theme.repo_list.dirty_marker,
        ratatui::style::Color::Yellow
    );
}

#[test]
fn test_load_with_env_resolves_muted_preset() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_dir = tmp.path().join("gitpane");
    fs::create_dir_all(&config_dir).unwrap();
    let cfg_path = config_dir.join(CONFIG_FILE);
    fs::write(&cfg_path, "theme = \"muted\"").unwrap();

    let env = MockEnv {
        xdg_config_home: Some(tmp.path().to_path_buf()),
        existing: HashSet::from([cfg_path.clone()]),
        ..Default::default()
    };
    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(
        config.theme.repo_list.dirty_marker,
        ratatui::style::Color::Indexed(178)
    );
}

#[test]
fn test_load_with_env_falls_back_to_default_for_unknown_theme() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_dir = tmp.path().join("gitpane");
    fs::create_dir_all(&config_dir).unwrap();
    let cfg_path = config_dir.join(CONFIG_FILE);
    fs::write(&cfg_path, "theme = \"nope\"").unwrap();

    let env = MockEnv {
        xdg_config_home: Some(tmp.path().to_path_buf()),
        existing: HashSet::from([cfg_path.clone()]),
        ..Default::default()
    };
    let config = Config::load_with_env(&env).unwrap();
    // Falls back to default; warn is logged but load does not error.
    assert_eq!(
        config.theme.repo_list.dirty_marker,
        ratatui::style::Color::Yellow
    );
}

#[test]
fn test_load_with_env_loads_custom_theme_next_to_gitpane_config_override() {
    // $GITPANE_CONFIG points to a config file in a non-XDG location.
    // A themes/ dir next to that file must be searched, otherwise
    // custom themes shipped alongside the override silently fall back
    // to the default.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_path = tmp.path().join("custom-config.toml");
    fs::write(&cfg_path, "theme = \"mine\"").unwrap();
    let themes_dir = tmp.path().join("themes");
    fs::create_dir_all(&themes_dir).unwrap();
    fs::write(
        themes_dir.join("mine.toml"),
        "[repo_list]\nstash = \"Magenta\"\n",
    )
    .unwrap();

    let env = MockEnv {
        gitpane_config: Some(cfg_path.clone()),
        existing: HashSet::from([cfg_path.clone()]),
        ..Default::default()
    };
    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.theme.repo_list.stash, ratatui::style::Color::Magenta);
}

#[test]
fn test_load_with_env_loads_custom_theme_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_dir = tmp.path().join("gitpane");
    fs::create_dir_all(&config_dir).unwrap();
    let cfg_path = config_dir.join(CONFIG_FILE);
    fs::write(&cfg_path, "theme = \"mine\"").unwrap();
    let themes_dir = config_dir.join("themes");
    fs::create_dir_all(&themes_dir).unwrap();
    fs::write(
        themes_dir.join("mine.toml"),
        "[repo_list]\nstash = \"Magenta\"\n",
    )
    .unwrap();

    let env = MockEnv {
        xdg_config_home: Some(tmp.path().to_path_buf()),
        existing: HashSet::from([cfg_path.clone()]),
        ..Default::default()
    };
    let config = Config::load_with_env(&env).unwrap();
    assert_eq!(config.theme.repo_list.stash, ratatui::style::Color::Magenta);
}
