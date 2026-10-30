use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

use crate::config::{Config, ConfigEnv, RealEnv};
use crate::theme::{discover_all_theme_names_bounded, load_theme_bounded};

use super::BridgeError;

#[cfg(test)]
mod tests;

fn color(value: Color) -> Value {
    let name = match value {
        Color::Reset => return json!({"type": "reset"}),
        Color::Indexed(value) => return json!({"type": "indexed", "value": value}),
        Color::Rgb(red, green, blue) => {
            return json!({"type": "rgb", "red": red, "green": green, "blue": blue});
        }
        Color::Black => "black",
        Color::Red => "red",
        Color::Green => "green",
        Color::Yellow => "yellow",
        Color::Blue => "blue",
        Color::Magenta => "magenta",
        Color::Cyan => "cyan",
        Color::Gray => "gray",
        Color::DarkGray => "dark_gray",
        Color::LightRed => "light_red",
        Color::LightGreen => "light_green",
        Color::LightYellow => "light_yellow",
        Color::LightBlue => "light_blue",
        Color::LightMagenta => "light_magenta",
        Color::LightCyan => "light_cyan",
        Color::White => "white",
    };
    json!({"type": "ansi", "name": name})
}

fn section(value: &impl Serialize) -> Result<Value, BridgeError> {
    let serialized = serde_json::to_value(value)
        .map_err(|error| BridgeError::new("theme_error", error.to_string()))?;
    let fields = serialized
        .as_object()
        .ok_or_else(|| BridgeError::new("theme_error", "Theme section is not an object."))?;
    let mut result = serde_json::Map::new();
    for (name, raw) in fields {
        let encoded = if raw.is_array() {
            let colors: Vec<Color> = serde_json::from_value(raw.clone())
                .map_err(|error| BridgeError::new("theme_error", error.to_string()))?;
            Value::Array(colors.into_iter().map(color).collect())
        } else {
            let parsed: Color = serde_json::from_value(raw.clone())
                .map_err(|error| BridgeError::new("theme_error", error.to_string()))?;
            color(parsed)
        };
        result.insert(name.clone(), encoded);
    }
    Ok(Value::Object(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct PreviewRequest {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct SaveRequest {
    name: String,
    expected_current: String,
    preview_token: String,
}

fn invalid(message: &str) -> BridgeError {
    BridgeError::new("invalid_request", message)
}
fn theme_error(error: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("theme_error", error.to_string())
}
fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Theme or configuration changed; reopen the picker.",
    )
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('\0')
        && Path::new(name).components().count() == 1
        && matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
        && !name.contains(['/', '\\'])
}

fn current(config: &Config, env: &dyn ConfigEnv) -> Result<Config, BridgeError> {
    if config.saved_snapshot.is_none() {
        return Ok(config.clone());
    }
    let mut loaded = Config::load_without_theme(env).map_err(theme_error)?;
    loaded.runtime_theme_override = config.runtime_theme_override.clone();
    Ok(loaded)
}

fn resolve(config: &Config, env: &dyn ConfigEnv, name: &str) -> Result<Value, BridgeError> {
    if !valid_name(name) {
        return Err(invalid("Invalid theme name."));
    }
    let theme = load_theme_bounded(name, &config.theme_dirs(env))
        .map_err(|error| BridgeError::new("theme_error", error.to_string()))?;
    Ok(json!({
        "name": name,
        "repo_list": section(&theme.repo_list)?,
        "file_list": section(&theme.file_list)?,
        "graph": section(&theme.graph)?,
    }))
}

fn token(theme: &Value) -> Result<String, BridgeError> {
    let encoded = serde_json::to_vec(theme).map_err(theme_error)?;
    Ok(crate::hash::hex(Sha256::digest(encoded).as_ref()))
}

pub(super) fn query(config: &Config) -> Result<Value, BridgeError> {
    let config = current(config, &RealEnv)?;
    resolve(&config, &RealEnv, config.effective_theme_name())
}

pub(super) fn list(config: &Config) -> Result<Value, BridgeError> {
    list_with_env(config, &RealEnv)
}

fn list_with_env(config: &Config, env: &dyn ConfigEnv) -> Result<Value, BridgeError> {
    let config = current(config, env)?;
    if config.theme_name.len() > 4096 || config.effective_theme_name().len() > 4096 {
        return Err(theme_error(
            "Configured theme name exceeds the supported limit.",
        ));
    }
    let names = discover_all_theme_names_bounded(&config.theme_dirs(env)).map_err(theme_error)?;
    Ok(json!({"current":config.effective_theme_name(),"persisted":config.theme_name,"names":names}))
}

pub(super) fn preview(config: &Config, input: PreviewRequest) -> Result<Value, BridgeError> {
    preview_with_env(config, input, &RealEnv)
}

fn preview_with_env(
    config: &Config,
    input: PreviewRequest,
    env: &dyn ConfigEnv,
) -> Result<Value, BridgeError> {
    let config = current(config, env)?;
    let theme = resolve(&config, env, &input.name)?;
    Ok(json!({"preview_token":token(&theme)?,"theme":theme}))
}

pub(super) fn save(config: &Config, input: SaveRequest) -> Result<Value, BridgeError> {
    save_with_env(config, input, &RealEnv)
}

fn save_with_env(
    config: &Config,
    input: SaveRequest,
    env: &dyn ConfigEnv,
) -> Result<Value, BridgeError> {
    if !valid_name(&input.name)
        || input.expected_current.len() > 4096
        || input.preview_token.len() != 64
        || !input
            .preview_token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("Invalid theme selection."));
    }
    if config.runtime_theme_override.is_some() {
        return Err(BridgeError::new(
            "theme_error",
            "A command-line theme override is active; restart Gitpane without --theme before saving a theme.",
        ));
    }
    let mut config = current(config, env)?;
    let theme = resolve(&config, env, &input.name)?;
    if token(&theme)? != input.preview_token {
        return Err(stale());
    }
    config.runtime_theme_override = None;
    config.theme_name = input.name.clone();
    if !config
        .save_theme_if_current(&input.expected_current)
        .map_err(theme_error)?
    {
        return Err(stale());
    }
    let saved = resolve(&config, env, &input.name).map_err(|error| {
        BridgeError::new(
            "outcome_unknown",
            format!(
                "Theme was saved, but could not be reloaded: {}",
                error.message
            ),
        )
    })?;
    if saved != theme {
        return Err(BridgeError::new(
            "outcome_unknown",
            "Theme was saved, but its file changed during the save.",
        ));
    }
    Ok(json!({"theme":saved}))
}
