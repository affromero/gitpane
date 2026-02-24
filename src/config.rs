use color_eyre::Result;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "default_root_dirs")]
    pub root_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub excluded_repos: Vec<String>,
    #[serde(default)]
    pub pinned_repos: Vec<PathBuf>,
    #[serde(default = "default_scan_depth")]
    pub scan_depth: usize,
    #[serde(default)]
    pub watch: WatchConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WatchConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Local status poll interval in seconds (fast, catches missed watcher events)
    #[serde(default = "default_poll_local_secs")]
    pub poll_local_secs: u64,
    /// Remote fetch poll interval in seconds (updates ahead/behind from origin)
    #[serde(default = "default_poll_fetch_secs")]
    pub poll_fetch_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UiConfig {
    #[serde(default = "default_frame_rate")]
    pub frame_rate: u16,
}

fn default_root_dirs() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|h| vec![h.join("Code")])
        .unwrap_or_default()
}

fn default_scan_depth() -> usize {
    2
}

fn default_debounce_ms() -> u64 {
    500
}

fn default_poll_local_secs() -> u64 {
    5
}

fn default_poll_fetch_secs() -> u64 {
    60
}

fn default_frame_rate() -> u16 {
    10
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            poll_local_secs: default_poll_local_secs(),
            poll_fetch_secs: default_poll_fetch_secs(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            frame_rate: default_frame_rate(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root_dirs: default_root_dirs(),
            excluded_repos: vec!["node_modules".into(), ".cargo".into()],
            pinned_repos: Vec::new(),
            scan_depth: default_scan_depth(),
            watch: WatchConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            let mut config: Config = toml::from_str(&contents)?;
            config.expand_tildes();
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        ProjectDirs::from("", "", "gitpane")
            .map(|dirs| dirs.config_dir().join("config.toml"))
            .unwrap_or_else(|| PathBuf::from("config.toml"))
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, contents)?;
        Ok(())
    }

    pub fn add_pinned_repo(&mut self, path: PathBuf) {
        if !self.pinned_repos.contains(&path) {
            self.pinned_repos.push(path);
        }
    }

    pub fn override_root(&mut self, root: PathBuf) {
        self.root_dirs = vec![root];
    }

    fn expand_tildes(&mut self) {
        if let Some(home) = dirs::home_dir() {
            for dir in &mut self.root_dirs {
                if dir.starts_with("~") {
                    *dir = home.join(dir.strip_prefix("~").unwrap());
                }
            }
            for dir in &mut self.pinned_repos {
                if dir.starts_with("~") {
                    *dir = home.join(dir.strip_prefix("~").unwrap());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
