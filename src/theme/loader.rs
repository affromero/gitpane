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

/// Discover themes with explicit errors and limits for editor-facing requests.
pub(crate) fn discover_all_theme_names_bounded(dirs: &[PathBuf]) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = BUILT_IN_THEMES.iter().map(|s| s.to_string()).collect();
    for custom in scan_custom_theme_names(dirs, true)? {
        if !names.contains(&custom) {
            names.push(custom);
        }
    }
    Ok(names)
}

/// Resolve a theme by name. Built-ins (`default`, `muted`) are returned
/// directly; any other name is treated as a custom theme file living at
/// `<dir>/gitpane/themes/<name>.toml` for each candidate `<dir>`.
pub(crate) fn load_theme(name: &str, candidate_dirs: &[PathBuf]) -> Result<Theme, LoadThemeError> {
    load_theme_inner(name, candidate_dirs, false)
}

/// Load the same theme format with a bounded, nonblocking file read.
pub(crate) fn load_theme_bounded(
    name: &str,
    candidate_dirs: &[PathBuf],
) -> Result<Theme, LoadThemeError> {
    load_theme_inner(name, candidate_dirs, true)
}

fn load_theme_inner(
    name: &str,
    candidate_dirs: &[PathBuf],
    bounded: bool,
) -> Result<Theme, LoadThemeError> {
    match name {
        "default" => return Ok(Theme::default()),
        "muted" => return Ok(muted()),
        _ => {}
    }

    let searched = candidate_paths(name, candidate_dirs);
    for path in &searched {
        if path.is_file() {
            return if bounded {
                load_theme_from_path_bounded(path)
            } else {
                load_theme_from_path(path)
            };
        }
    }

    Err(LoadThemeError::Unknown {
        name: name.to_string(),
        builtins: BUILT_IN_THEMES.to_vec(),
        custom: if bounded {
            Vec::new()
        } else {
            discover_custom_theme_names(candidate_dirs)
        },
        searched,
    })
}

fn load_theme_from_path_bounded(path: &Path) -> Result<Theme, LoadThemeError> {
    use std::io::Read;
    let failed = |error: String| LoadThemeError::InvalidFile {
        path: path.to_owned(),
        message: error,
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| failed(error.to_string()))?;
    let metadata = file.metadata().map_err(|error| failed(error.to_string()))?;
    const LIMIT: u64 = 1024 * 1024;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(failed(
            "theme must be a regular file no larger than 1 MiB".into(),
        ));
    }
    let mut contents = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut contents)
        .map_err(|error| failed(error.to_string()))?;
    if contents.len() as u64 > LIMIT {
        return Err(failed("theme exceeds the 1 MiB limit".into()));
    }
    let contents = std::str::from_utf8(&contents).map_err(|error| failed(error.to_string()))?;
    toml::from_str(contents).map_err(|error: toml::de::Error| failed(error.to_string()))
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
    scan_custom_theme_names(dirs, false).unwrap_or_default()
}

fn scan_custom_theme_names(dirs: &[PathBuf], strict: bool) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut count = 0usize;
    for dir in dirs {
        let themes_dir = dir.join("themes");
        let entries = match std::fs::read_dir(&themes_dir) {
            Ok(entries) => entries,
            Err(error) if !strict || error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for item in entries {
            let entry = match item {
                Ok(entry) => entry,
                Err(_) if !strict => continue,
                Err(error) => return Err(error),
            };
            count += 1;
            if strict && count > 4096 {
                return Err(std::io::Error::other("too many theme directory entries"));
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let owned = stem.to_string();
                if strict && (owned.len() > 255 || names.len() >= 512) {
                    return Err(std::io::Error::other(
                        "theme discovery exceeds the supported limit",
                    ));
                }
                if !names.contains(&owned) {
                    names.push(owned);
                }
            }
        }
    }
    names.sort();
    Ok(names)
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
        let expected_path = dir.path().join("themes").join("nope.toml");
        let LoadThemeError::Unknown { name, searched, .. } = &err else {
            panic!("expected an unknown-theme error: {err}");
        };
        assert_eq!(name, "nope");
        assert_eq!(searched, std::slice::from_ref(&expected_path));
        let msg = err.to_string();
        assert!(
            msg.contains(&expected_path.display().to_string()),
            "got: {msg}"
        );
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
