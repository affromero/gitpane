//! Theme name resolution and loading through `load_with_env`.

use super::*;

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
