use ratatui::style::Color;
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{Config, RealEnv};
use crate::theme::load_theme;

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

pub(super) fn query(config: &Config) -> Result<Value, BridgeError> {
    let name = config.effective_theme_name();
    let theme = load_theme(name, &config.theme_dirs(&RealEnv))
        .map_err(|error| BridgeError::new("theme_error", error.to_string()))?;
    Ok(json!({
        "name": name,
        "repo_list": section(&theme.repo_list)?,
        "file_list": section(&theme.file_list)?,
        "graph": section(&theme.graph)?,
    }))
}
