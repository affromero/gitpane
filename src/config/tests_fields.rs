//! Per-section defaults, TOML roundtrips, and scan-root resolution.

use super::terminal::{TERMINAL_GOTOS, goto_command_for_env};
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

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
    // The override wins for this run, but the configured roots stay intact.
    assert_eq!(
        config.effective_root_dirs().as_ref(),
        &[PathBuf::from("/tmp/my-repos")],
    );
    assert_ne!(config.root_dirs, vec![PathBuf::from("/tmp/my-repos")]);
}

/// Missing roots are exactly the configured ones that don't exist on disk.
/// Discovery silently skips them, so they must be discoverable for the
/// startup warning and diagnostics.
#[test]
fn test_missing_roots_reports_only_non_existent_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().join("code");
    fs::create_dir_all(&existing).unwrap();
    let missing = tmp.path().join("work");

    let config = Config {
        root_dirs: vec![existing.clone(), missing.clone()],
        ..Config::default()
    };

    let mut got = config.missing_roots();
    got.sort();
    assert_eq!(got, vec![missing]);
}

/// A root that exists is never reported as missing even when it contains no
/// repos — existence is the whole signal, not discoverability.
#[test]
fn test_missing_roots_ignores_existing_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().join("empty-code");
    fs::create_dir_all(&existing).unwrap();
    let config = Config {
        root_dirs: vec![existing.clone()],
        ..Config::default()
    };
    assert!(config.missing_roots().is_empty());
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
fn test_herdr_forward_right_click_defaults_false() {
    let config: Config = toml::from_str("").unwrap();
    assert!(!config.herdr.forward_right_click);
}

#[test]
fn test_herdr_forward_right_click_roundtrip() {
    let mut config = Config::default();
    config.herdr.forward_right_click = true;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert!(loaded.herdr.forward_right_click);
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
fn test_git_config_defaults() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.git.op_timeout_secs, 300);
    assert_eq!(Config::default().git.op_timeout_secs, 300);
}

#[test]
fn test_git_config_roundtrip() {
    let mut config = Config::default();
    config.git.op_timeout_secs = 90;
    let serialized = toml::to_string_pretty(&config).unwrap();
    let loaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(loaded.git.op_timeout_secs, 90);
}

#[test]
fn test_git_config_parse() {
    let toml_str = r#"
        [git]
        op_timeout_secs = 120
    "#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.git.op_timeout_secs, 120);
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
fn test_effective_root_dirs_prefers_runtime_override() {
    let mut config = Config {
        root_dirs: vec![PathBuf::from("/config/roots")],
        ..Default::default()
    };
    assert_eq!(
        config.effective_root_dirs().as_ref(),
        &[PathBuf::from("/config/roots")],
        "without an override the configured roots win",
    );

    config.override_root(PathBuf::from("/tmp/run-root"));
    assert_eq!(
        config.effective_root_dirs().as_ref(),
        &[PathBuf::from("/tmp/run-root")],
        "an override must replace the configured roots",
    );
}

#[test]
fn test_runtime_root_override_is_never_serialized() {
    let mut config = Config {
        root_dirs: vec![PathBuf::from("/config/roots")],
        ..Default::default()
    };
    config.override_root(PathBuf::from("/tmp/run-root"));

    // Saving the whole config (triggered by pin/remove/rescan/theme actions)
    // must not leak the run-local override, and must not rewrite root_dirs.
    let serialized = toml::to_string_pretty(&config).unwrap();
    let reloaded: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(reloaded.runtime_root_override, None);
    assert_eq!(reloaded.root_dirs, vec![PathBuf::from("/config/roots")]);
}

#[test]
fn test_missing_roots_follows_the_runtime_override() {
    // The missing-root hint and the scan-root override are tied together by
    // one line in Config::missing_roots: it must report the override's
    // missing root, not the configured-but-unused one.
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().join("configured");
    fs::create_dir_all(&existing).unwrap();
    let mut config = Config {
        root_dirs: vec![existing],
        ..Default::default()
    };
    let absent = tmp.path().join("override-gone");
    config.override_root(absent.clone());
    assert_eq!(config.missing_roots(), vec![absent]);
}

#[test]
fn test_override_root_expands_tilde() {
    let mut config = Config::default();
    config.override_root(PathBuf::from("~/Code"));
    let roots = config.effective_root_dirs();
    assert!(
        !roots.iter().any(|r| r.starts_with("~")),
        "tilde must expand"
    );
}

/// An existing config with a `[ui]` section written before this key existed
/// still gets worktrees expanded, rather than deserializing to `false`.
#[test]
fn test_expand_worktrees_defaults_on_for_configs_without_the_key() {
    let config: Config = toml::from_str("[ui]\nframe_rate = 30\n").unwrap();
    assert!(config.ui.expand_worktrees);

    let config: Config = toml::from_str("[ui]\nexpand_worktrees = false\n").unwrap();
    assert!(!config.ui.expand_worktrees);
}
