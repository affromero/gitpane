use std::fs;
use std::io::Cursor;

use ratatui::style::Color;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::config::Config;
use crate::theme::Theme;

use super::super::{BridgeOptions, serve_inner};

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
