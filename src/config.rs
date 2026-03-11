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
    #[serde(default)]
    pub graph: GraphConfig,
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
    /// Max concurrent poll tasks (limits CPU usage with many repos)
    #[serde(default = "default_max_concurrent_polls")]
    pub max_concurrent_polls: usize,
    /// Directory names to ignore in watcher events (reduces noise)
    #[serde(default = "default_watch_exclude_dirs")]
    pub watch_exclude_dirs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UpdatePosition {
    #[default]
    TopRight,
    TopLeft,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UiConfig {
    #[serde(default = "default_frame_rate")]
    pub frame_rate: u16,
    #[serde(default = "default_check_for_updates")]
    pub check_for_updates: bool,
    #[serde(default)]
    pub update_position: UpdatePosition,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BranchFilter {
    #[default]
    All,
    Local,
    Remote,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GraphConfig {
    #[serde(default)]
    pub branches: BranchFilter,
    #[serde(default = "default_label_max_len")]
    pub label_max_len: usize,
    #[serde(default = "default_show_stats")]
    pub show_stats: bool,
}

fn default_show_stats() -> bool {
    true
}

fn default_label_max_len() -> usize {
    24
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            branches: BranchFilter::default(),
            label_max_len: default_label_max_len(),
            show_stats: default_show_stats(),
        }
    }
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
    30
}

fn default_max_concurrent_polls() -> usize {
    4
}

fn default_watch_exclude_dirs() -> Vec<String> {
    [
        "node_modules",
        "target",
        ".build",
        "dist",
        "vendor",
        ".venv",
        "__pycache__",
        ".next",
        "Pods",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_frame_rate() -> u16 {
    10
}

fn default_check_for_updates() -> bool {
    true
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            poll_local_secs: default_poll_local_secs(),
            poll_fetch_secs: default_poll_fetch_secs(),
            max_concurrent_polls: default_max_concurrent_polls(),
            watch_exclude_dirs: default_watch_exclude_dirs(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            frame_rate: default_frame_rate(),
            check_for_updates: default_check_for_updates(),
            update_position: UpdatePosition::default(),
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
            graph: GraphConfig::default(),
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
    fn test_max_concurrent_polls_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.watch.max_concurrent_polls, 4);
    }

    #[test]
    fn test_watch_exclude_dirs_default() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.watch.watch_exclude_dirs.contains(&"node_modules".to_string()));
        assert!(config.watch.watch_exclude_dirs.contains(&"target".to_string()));
        assert!(config.watch.watch_exclude_dirs.contains(&".next".to_string()));
    }
}
