//! Config discovery: which file wins, where a save lands, keybinding parsing.

use super::load::{LoadResolution, candidate_search_paths, default_write_path, resolve_load};
use super::test_support::*;
use super::*;
use std::{collections::HashSet, fs, path::PathBuf};

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
