use std::fs;
use std::io::Cursor;

use ratatui::style::Color;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::config::Config;
use crate::theme::Theme;

use super::super::{BridgeOptions, serve_inner};

struct ThemeEnv(std::path::PathBuf);

impl crate::config::ConfigEnv for ThemeEnv {
    fn gitpane_config(&self) -> Option<std::path::PathBuf> {
        Some(self.0.join("config.toml"))
    }
    fn xdg_config_home(&self) -> Option<std::path::PathBuf> {
        None
    }
    fn home_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
    fn project_config_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
    fn file_exists(&self, path: &std::path::Path) -> bool {
        path.exists()
    }
}

#[test]
fn picker_previews_without_saving_and_commits_with_stale_guards() {
    use super::{PreviewRequest, SaveRequest, list_with_env, preview_with_env, save_with_env};
    let temp = TempDir::new().unwrap();
    let env = ThemeEnv(temp.path().to_owned());
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "theme = 'default'\nscan_depth = 2\n").unwrap();
    let config = Config::load_without_theme(&env).unwrap();
    fs::create_dir(temp.path().join("themes")).unwrap();
    fs::write(
        temp.path().join("themes/custom.toml"),
        toml::to_string(&Theme::default()).unwrap(),
    )
    .unwrap();
    let list = list_with_env(&config, &env).unwrap();
    assert_eq!(list["current"], "default");
    assert_eq!(list["persisted"], "default");
    assert_eq!(list["names"], json!(["default", "muted", "custom"]));
    let preview = preview_with_env(
        &config,
        PreviewRequest {
            name: "custom".into(),
        },
        &env,
    )
    .unwrap();
    assert_eq!(preview["theme"]["name"], "custom");
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        "theme = 'default'\nscan_depth = 2\n"
    );
    let token = preview["preview_token"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    let mut override_config = config.clone();
    override_config.runtime_theme_override = Some("muted".into());
    assert_eq!(
        list_with_env(&override_config, &env).unwrap()["current"],
        "muted"
    );
    let failure = save_with_env(
        &override_config,
        SaveRequest {
            name: "custom".into(),
            expected_current: "default".into(),
            preview_token: token.clone(),
        },
        &env,
    )
    .unwrap_err();
    assert_eq!(failure.code, "theme_error");
    assert!(failure.message.contains("--theme"));
    fs::write(&config_path, "theme = 'default'\nscan_depth = 9\n").unwrap();
    let saved = save_with_env(
        &config,
        SaveRequest {
            name: "custom".into(),
            expected_current: "default".into(),
            preview_token: token.clone(),
        },
        &env,
    )
    .unwrap();
    assert_eq!(saved["theme"], preview["theme"]);
    let disk = Config::load_without_theme(&env).unwrap();
    assert_eq!(disk.theme_name, "custom");
    assert_eq!(disk.scan_depth, 9);
    assert_eq!(list_with_env(&config, &env).unwrap()["current"], "custom");
    let stale = save_with_env(
        &config,
        SaveRequest {
            name: "custom".into(),
            expected_current: "default".into(),
            preview_token: token.clone(),
        },
        &env,
    )
    .unwrap_err();
    assert_eq!(stale.code, "stale_target");
    let mut changed = Theme::default();
    changed.repo_list.dirty_marker = Color::Rgb(1, 2, 3);
    fs::write(
        temp.path().join("themes/custom.toml"),
        toml::to_string(&changed).unwrap(),
    )
    .unwrap();
    let stale = save_with_env(
        &config,
        SaveRequest {
            name: "custom".into(),
            expected_current: "custom".into(),
            preview_token: token,
        },
        &env,
    )
    .unwrap_err();
    assert_eq!(stale.code, "stale_target");
    assert_eq!(
        Config::load_without_theme(&env).unwrap().theme_name,
        "custom"
    );
}

#[test]
fn picker_rejects_bad_themes_names_and_oversized_files() {
    use super::{PreviewRequest, list_with_env, preview_with_env};
    let temp = TempDir::new().unwrap();
    let env = ThemeEnv(temp.path().to_owned());
    fs::write(temp.path().join("config.toml"), "theme = 'default'\n").unwrap();
    let config = Config::load_without_theme(&env).unwrap();
    fs::write(temp.path().join("themes"), "not a directory").unwrap();
    assert_eq!(
        list_with_env(&config, &env).unwrap_err().code,
        "theme_error"
    );
    fs::remove_file(temp.path().join("themes")).unwrap();
    fs::create_dir(temp.path().join("themes")).unwrap();
    for name in ["../outside", "a/b", "a\\b", ""] {
        assert_eq!(
            preview_with_env(&config, PreviewRequest { name: name.into() }, &env)
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    fs::write(
        temp.path().join("themes/bad.toml"),
        "[repo_list]\nstash='bogus'\n",
    )
    .unwrap();
    assert_eq!(
        preview_with_env(&config, PreviewRequest { name: "bad".into() }, &env)
            .unwrap_err()
            .code,
        "theme_error"
    );
    fs::write(
        temp.path().join("themes/large.toml"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    assert_eq!(
        preview_with_env(
            &config,
            PreviewRequest {
                name: "large".into()
            },
            &env
        )
        .unwrap_err()
        .code,
        "theme_error"
    );
    fs::write(temp.path().join("config.toml"), "[broken\n").unwrap();
    assert_eq!(
        list_with_env(&config, &env).unwrap_err().code,
        "theme_error"
    );
}

#[test]
fn picker_repairs_invalid_persisted_theme_name() {
    use super::{PreviewRequest, SaveRequest, list_with_env, preview_with_env, save_with_env};
    let temp = TempDir::new().unwrap();
    let env = ThemeEnv(temp.path().to_owned());
    let path = temp.path().join("config.toml");
    fs::write(&path, "theme = '../bad'\n").unwrap();
    let config = Config::load_without_theme(&env).unwrap();
    let list = list_with_env(&config, &env).unwrap();
    assert_eq!(list["persisted"], "../bad");
    let preview = preview_with_env(
        &config,
        PreviewRequest {
            name: "default".into(),
        },
        &env,
    )
    .unwrap();
    let saved = save_with_env(
        &config,
        SaveRequest {
            name: "default".into(),
            expected_current: "../bad".into(),
            preview_token: preview["preview_token"].as_str().unwrap().into(),
        },
        &env,
    )
    .unwrap();
    assert_eq!(saved["theme"]["name"], "default");
    assert_eq!(
        Config::load_without_theme(&env).unwrap().theme_name,
        "default"
    );
}

fn request(config: &Config, params: Value) -> Value {
    let input = format!(
        "{}\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"version": 1, "workspace_roots": []}}),
        json!({"id": 2, "method": "theme", "params": params}),
    );
    let options = BridgeOptions {
        submodules: &config.submodules,
        graph: &config.graph,
        worktree: &config.worktree,
        review_base: config.review.base.as_deref(),
        theme: config,
        monitor_stdio: false,
    };
    let mut output = Vec::new();
    serve_inner(&mut Cursor::new(input), &mut output, &options, false, false).unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    replies[1].clone()
}

#[test]
fn default_and_muted_themes_keep_native_semantic_colors_and_palettes() {
    let mut config = Config::default();
    let default = request(&config, json!({}));
    assert_eq!(default["result"]["name"], "default");
    assert_eq!(
        default["result"]["repo_list"]["dirty_marker"],
        json!({"type": "ansi", "name": "yellow"})
    );
    assert_eq!(
        default["result"]["file_list"]["status_added"],
        json!({"type": "ansi", "name": "green"})
    );
    assert_eq!(
        default["result"]["graph"]["lane_palette"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        default["result"]["graph"]["author_palette"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    config.theme_name = "muted".to_string();
    let muted = request(&config, json!({}));
    assert_eq!(muted["result"]["name"], "muted");
    assert_eq!(
        muted["result"]["repo_list"]["dirty_marker"],
        json!({"type": "indexed", "value": 178})
    );
    assert_ne!(
        default["result"]["graph"]["lane_palette"],
        muted["result"]["graph"]["lane_palette"]
    );
    assert_eq!(
        request(&config, json!({"extra": true}))["error"]["code"],
        "invalid_request"
    );
}

#[test]
fn custom_theme_preserves_color_variants_and_empty_palettes() {
    let temp = TempDir::new().unwrap();
    let themes = temp.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    let mut theme = Theme::default();
    theme.repo_list.dirty_marker = Color::Reset;
    theme.repo_list.stash = Color::Indexed(255);
    theme.file_list.status_modified = Color::Rgb(0, 128, 255);
    theme.graph.lane_palette = Vec::new();
    theme.graph.author_palette = vec![Color::LightBlue, Color::Rgb(12, 34, 56)];
    fs::write(
        themes.join("bridge-fixture.toml"),
        toml::to_string(&theme).unwrap(),
    )
    .unwrap();
    let mut config = Config {
        theme_name: "bridge-fixture".to_owned(),
        loaded_path: Some(temp.path().join("config.toml")),
        ..Config::default()
    };
    let response = request(&config, json!({}));
    let result = &response["result"];
    assert_eq!(result["name"], "bridge-fixture");
    assert_eq!(
        result["repo_list"]["dirty_marker"],
        json!({"type": "reset"})
    );
    assert_eq!(
        result["repo_list"]["stash"],
        json!({"type": "indexed", "value": 255})
    );
    assert_eq!(
        result["file_list"]["status_modified"],
        json!({"type": "rgb", "red": 0, "green": 128, "blue": 255})
    );
    assert_eq!(result["graph"]["lane_palette"], json!([]));
    assert_eq!(
        result["graph"]["author_palette"],
        json!([
            {"type": "ansi", "name": "light_blue"},
            {"type": "rgb", "red": 12, "green": 34, "blue": 56}
        ])
    );

    fs::write(
        themes.join("bridge-fixture.toml"),
        "[repo_list]\nstash = 'bogus'\n",
    )
    .unwrap();
    let invalid = request(&config, json!({}));
    assert_eq!(invalid["error"]["code"], "theme_error");
    assert!(
        invalid["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bridge-fixture.toml")
    );
    config.theme_name = "missing-bridge-fixture".to_owned();
    let missing = request(&config, json!({}));
    assert_eq!(missing["error"]["code"], "theme_error");
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing-bridge-fixture")
    );
}
