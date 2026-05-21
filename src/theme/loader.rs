//! Theme resolution from a name: built-in preset or custom file at
//! `<config_dir>/gitpane/themes/<name>.toml`.

use std::path::{Path, PathBuf};

use super::{Theme, muted};

#[derive(Debug)]
pub(crate) enum LoadThemeError {
    /// Name didn't match any built-in or custom theme on disk.
    Unknown {
        name: String,
        builtins: Vec<&'static str>,
        searched: Vec<PathBuf>,
        custom: Vec<String>,
    },
    /// Custom theme file exists but failed to parse or read.
    InvalidFile { path: PathBuf, message: String },
}

impl std::fmt::Display for LoadThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown {
                name,
                builtins,
                searched,
                custom,
            } => {
                write!(
                    f,
                    "unknown theme '{name}'. Built-in: {}",
                    builtins.join(", ")
                )?;
                if !custom.is_empty() {
                    write!(
                        f,
                        ". Custom (in {}): {}",
                        searched
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        custom.join(", ")
                    )?;
                } else if !searched.is_empty() {
                    write!(
                        f,
                        ". Searched: {}",
                        searched
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                Ok(())
            }
            Self::InvalidFile { path, message } => {
                write!(f, "failed to load theme file {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for LoadThemeError {}

pub(crate) const BUILT_IN_THEMES: &[&str] = &["default", "muted"];

/// All theme names available: built-ins followed by custom files
/// discovered under each `<dir>/themes/`. Built-ins keep their declared
/// order; customs are sorted alphabetically and de-duplicated.
pub(crate) fn discover_all_theme_names(dirs: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = BUILT_IN_THEMES.iter().map(|s| s.to_string()).collect();
    for custom in discover_custom_theme_names(dirs) {
        if !names.contains(&custom) {
            names.push(custom);
        }
    }
    names
}

/// Resolve a theme by name. Built-ins (`default`, `muted`) are returned
/// directly; any other name is treated as a custom theme file living at
/// `<dir>/gitpane/themes/<name>.toml` for each candidate `<dir>`.
pub(crate) fn load_theme(name: &str, candidate_dirs: &[PathBuf]) -> Result<Theme, LoadThemeError> {
    match name {
        "default" => return Ok(Theme::default()),
        "muted" => return Ok(muted()),
        _ => {}
    }

    let searched = candidate_paths(name, candidate_dirs);
    for path in &searched {
        if path.is_file() {
            return load_theme_from_path(path);
        }
    }

    Err(LoadThemeError::Unknown {
        name: name.to_string(),
        builtins: BUILT_IN_THEMES.to_vec(),
        custom: discover_custom_theme_names(candidate_dirs),
        searched,
    })
}

fn load_theme_from_path(path: &Path) -> Result<Theme, LoadThemeError> {
    let contents = std::fs::read_to_string(path).map_err(|e| LoadThemeError::InvalidFile {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    toml::from_str(&contents).map_err(|e| LoadThemeError::InvalidFile {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn candidate_paths(name: &str, dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|d| d.join("themes").join(format!("{name}.toml")))
        .collect()
}

fn discover_custom_theme_names(dirs: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for dir in dirs {
        let themes_dir = dir.join("themes");
        let Ok(entries) = std::fs::read_dir(&themes_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let owned = stem.to_string();
                if !names.contains(&owned) {
                    names.push(owned);
                }
            }
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_default_returns_default_theme() {
        let theme = load_theme("default", &[]).unwrap();
        assert_eq!(theme.repo_list.dirty_marker, ratatui::style::Color::Yellow);
    }

    #[test]
    fn load_muted_returns_muted_preset() {
        let theme = load_theme("muted", &[]).unwrap();
        assert_eq!(
            theme.repo_list.dirty_marker,
            ratatui::style::Color::Indexed(178)
        );
    }

    #[test]
    fn unknown_theme_reports_searched_paths() {
        let dir = TempDir::new().unwrap();
        let candidates = vec![dir.path().to_path_buf()];
        let err = load_theme("nope", &candidates).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown theme 'nope'"), "got: {msg}");
        assert!(
            msg.contains("default"),
            "expected built-ins in error: {msg}"
        );
        assert!(msg.contains("muted"), "expected built-ins in error: {msg}");
    }

    #[test]
    fn custom_theme_file_loads_and_overrides_default() {
        let dir = TempDir::new().unwrap();
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir(&themes_dir).unwrap();
        let theme_path = themes_dir.join("mine.toml");
        std::fs::write(&theme_path, "[repo_list]\nstash = \"Magenta\"\n").unwrap();

        let theme = load_theme("mine", &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(theme.repo_list.stash, ratatui::style::Color::Magenta);
        // Untouched slots stay at the default.
        assert_eq!(theme.repo_list.dirty_marker, ratatui::style::Color::Yellow);
    }

    #[test]
    fn custom_theme_with_invalid_toml_returns_parse_error() {
        let dir = TempDir::new().unwrap();
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir(&themes_dir).unwrap();
        let theme_path = themes_dir.join("broken.toml");
        std::fs::write(&theme_path, "[oops nope").unwrap();

        let err = load_theme("broken", &[dir.path().to_path_buf()]).unwrap_err();
        assert!(matches!(err, LoadThemeError::InvalidFile { .. }));
    }

    #[test]
    fn discover_lists_custom_theme_names_alphabetically() {
        let dir = TempDir::new().unwrap();
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("zeta.toml"), "").unwrap();
        std::fs::write(themes_dir.join("alpha.toml"), "").unwrap();
        std::fs::write(themes_dir.join("not-a-theme.json"), "").unwrap();

        let names = discover_custom_theme_names(&[dir.path().to_path_buf()]);
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn discover_all_includes_builtins_then_customs() {
        let dir = TempDir::new().unwrap();
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("zeta.toml"), "").unwrap();
        std::fs::write(themes_dir.join("alpha.toml"), "").unwrap();

        let names = discover_all_theme_names(&[dir.path().to_path_buf()]);
        assert_eq!(names, vec!["default", "muted", "alpha", "zeta"]);
    }

    #[test]
    fn discover_all_dedupes_custom_named_like_builtin() {
        let dir = TempDir::new().unwrap();
        let themes_dir = dir.path().join("themes");
        std::fs::create_dir(&themes_dir).unwrap();
        // A custom named "muted" should not appear twice; the built-in wins
        // for positioning, the file is ignored.
        std::fs::write(themes_dir.join("muted.toml"), "").unwrap();
        let names = discover_all_theme_names(&[dir.path().to_path_buf()]);
        assert_eq!(names.iter().filter(|n| *n == "muted").count(), 1);
    }
}
