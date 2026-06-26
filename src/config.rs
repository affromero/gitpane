use color_eyre::{Result, eyre::eyre};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::theme::{LoadThemeError, Theme, load_theme};

const APP_NAME: &str = "gitpane";
const CONFIG_FILE: &str = "config.toml";

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
    #[serde(default)]
    pub submodules: SubmoduleConfig,
    #[serde(default)]
    pub open: OpenConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub worktree: WorktreeConfig,
    /// Name of the active theme. Built-in: "default" or "muted". Any other
    /// value loads `<config_dir>/gitpane/themes/<name>.toml`.
    #[serde(default = "default_theme_name", rename = "theme")]
    pub theme_name: String,
    #[serde(skip, default)]
    pub theme: Theme,
    /// Session-only theme override (set by `--theme`). Not serialized; the
    /// `theme_name` field on disk is preserved across saves while this is
    /// active. The picker reads this via `effective_theme_name()`.
    #[serde(skip, default)]
    pub runtime_theme_override: Option<String>,
    #[serde(skip, default)]
    pub(crate) loaded_path: Option<PathBuf>,
    #[serde(skip, default)]
    pub(crate) write_target_override: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WatchConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Minimum milliseconds between watcher-triggered local status refreshes
    /// for the same repo.
    #[serde(default = "default_refresh_cooldown_ms")]
    pub refresh_cooldown_ms: u64,
    /// Install watches inside each repo worktree. Disabled by default because
    /// busy generated or training files can saturate Linux inotify even when
    /// status refreshes are throttled. Local polling still catches changes.
    #[serde(default = "default_watch_worktree_dirs")]
    pub watch_worktree_dirs: bool,
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
    /// Minimum seconds between two auto-rescans triggered by root-dir
    /// filesystem changes. Higher values reduce wasted scans during long
    /// operations like `git clone`; lower values shorten the delay before
    /// a newly-cloned repo appears.
    #[serde(default = "default_discovery_cooldown_secs")]
    pub discovery_cooldown_secs: u64,
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
    /// Mark repos/worktrees that have a live tmux pane cwd'd inside them.
    #[serde(default = "default_show_liveness")]
    pub show_liveness: bool,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SubmoduleConfig {
    #[serde(default)]
    pub ignore_dirty: bool,
    #[serde(default = "default_warn_unpushed")]
    pub warn_unpushed: bool,
}

impl Default for SubmoduleConfig {
    fn default() -> Self {
        Self {
            ignore_dirty: false,
            warn_unpushed: default_warn_unpushed(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpenConfig {
    /// Command run by `o` to open the selected repo/worktree. `{path}` is the
    /// target directory. How it runs depends on `placement` (below). Unset opens
    /// a tmux pane (a shell) when `placement` is the default `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Where/how to launch `command`. `command` (default): the command IS the
    /// launcher, run directly (e.g. `cursor {path}`); empty opens a tmux pane.
    /// `split-window`/`new-window` (+ tmux flags like `-h`/`-t <name>`): gitpane
    /// runs it in a tmux pane/window. `inline`: suspend gitpane and run it here.
    /// `ask`: pick placement interactively. Outside tmux, a tmux placement runs
    /// inline.
    #[serde(default = "default_open_placement")]
    pub placement: String,
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            command: None,
            placement: default_open_placement(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReviewConfig {
    /// Command run by `v` to review the selected repo/worktree's changes.
    /// `{base}` is the resolved base ref, `{path}` the directory. When unset,
    /// defaults to `git diff {base}...HEAD`; pipe through a viewer for nicer
    /// output, e.g. `git diff {base}...HEAD | delta --side-by-side`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Base ref to diff against, e.g. `origin/main`. When unset, gitpane
    /// resolves the repository's default branch (`origin/HEAD` → main → master).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Where/how to launch the review command. Same vocabulary as `[open]
    /// placement`; defaults to `new-window` (a new tmux window).
    #[serde(default = "default_review_placement")]
    pub placement: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            command: None,
            base: None,
            placement: default_review_placement(),
        }
    }
}

fn default_open_placement() -> String {
    "command".to_string()
}

fn default_review_placement() -> String {
    "new-window".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct WorktreeConfig {
    /// Directory new worktrees are created under, each as `<repo>-<branch>`.
    /// When unset, a worktree is created as a sibling of its repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// Filesystem path for a new worktree of `repo_path` on `branch`: `<dir>` (or
/// the repo's parent when `dir` is unset) joined with `<repo_name>-<branch>`.
/// Branch slashes become dashes so the directory name stays single-segment.
pub(crate) fn worktree_path(config: &WorktreeConfig, repo_path: &Path, branch: &str) -> PathBuf {
    let repo_name = repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let dir_name = format!("{repo_name}-{}", branch.replace('/', "-"));
    match &config.dir {
        Some(base) => base.join(dir_name),
        None => repo_path.parent().unwrap_or(repo_path).join(dir_name),
    }
}

fn default_warn_unpushed() -> bool {
    true
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

fn default_theme_name() -> String {
    "default".into()
}

fn default_debounce_ms() -> u64 {
    500
}

fn default_refresh_cooldown_ms() -> u64 {
    5000
}

fn default_watch_worktree_dirs() -> bool {
    false
}

fn default_discovery_cooldown_secs() -> u64 {
    5
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

fn default_show_liveness() -> bool {
    true
}

fn default_check_for_updates() -> bool {
    true
}

pub(crate) trait ConfigEnv {
    fn gitpane_config(&self) -> Option<PathBuf>;
    fn xdg_config_home(&self) -> Option<PathBuf>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn project_config_dir(&self) -> Option<PathBuf>;
    fn file_exists(&self, path: &Path) -> bool;
}

pub(crate) struct RealEnv;

impl ConfigEnv for RealEnv {
    fn gitpane_config(&self) -> Option<PathBuf> {
        std::env::var_os("GITPANE_CONFIG")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn xdg_config_home(&self) -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn project_config_dir(&self) -> Option<PathBuf> {
        ProjectDirs::from("", "", APP_NAME).map(|dirs| dirs.config_dir().to_path_buf())
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LoadResolution {
    EnvOverride(PathBuf),
    SearchOrder(Vec<PathBuf>),
}

fn resolve_load(env: &dyn ConfigEnv) -> LoadResolution {
    if let Some(path) = env.gitpane_config() {
        return LoadResolution::EnvOverride(path);
    }

    LoadResolution::SearchOrder(candidate_search_paths(env))
}

fn xdg_config_path(config_home: PathBuf) -> PathBuf {
    config_home.join(APP_NAME).join(CONFIG_FILE)
}

fn dot_config_path(home: PathBuf) -> PathBuf {
    home.join(".config").join(APP_NAME).join(CONFIG_FILE)
}

fn candidate_search_paths(env: &dyn ConfigEnv) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(xdg) = env.xdg_config_home() {
        paths.push(xdg_config_path(xdg));
    }
    if let Some(home) = env.home_dir() {
        paths.push(dot_config_path(home));
    }
    if let Some(project_dir) = env.project_config_dir() {
        paths.push(project_dir.join(CONFIG_FILE));
    }

    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

fn default_write_path(env: &dyn ConfigEnv) -> Option<PathBuf> {
    env.xdg_config_home()
        .map(xdg_config_path)
        .or_else(|| env.home_dir().map(dot_config_path))
        .or_else(|| env.project_config_dir().map(|dir| dir.join(CONFIG_FILE)))
}

/// Directories that may host a `themes/<name>.toml` file. Mirrors
/// `candidate_search_paths` but strips the trailing `config.toml`.
pub(crate) fn candidate_theme_dirs(env: &dyn ConfigEnv) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(xdg) = env.xdg_config_home() {
        dirs.push(xdg.join(APP_NAME));
    }
    if let Some(home) = env.home_dir() {
        dirs.push(home.join(".config").join(APP_NAME));
    }
    if let Some(project_dir) = env.project_config_dir() {
        dirs.push(project_dir);
    }
    let mut seen = HashSet::new();
    dirs.retain(|p| seen.insert(p.clone()));
    dirs
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            refresh_cooldown_ms: default_refresh_cooldown_ms(),
            watch_worktree_dirs: default_watch_worktree_dirs(),
            poll_local_secs: default_poll_local_secs(),
            poll_fetch_secs: default_poll_fetch_secs(),
            max_concurrent_polls: default_max_concurrent_polls(),
            watch_exclude_dirs: default_watch_exclude_dirs(),
            discovery_cooldown_secs: default_discovery_cooldown_secs(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            frame_rate: default_frame_rate(),
            check_for_updates: default_check_for_updates(),
            update_position: UpdatePosition::default(),
            show_liveness: default_show_liveness(),
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
            submodules: SubmoduleConfig::default(),
            open: OpenConfig::default(),
            review: ReviewConfig::default(),
            worktree: WorktreeConfig::default(),
            theme_name: default_theme_name(),
            theme: Theme::default(),
            runtime_theme_override: None,
            loaded_path: None,
            write_target_override: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_with_env(&RealEnv)
    }

    #[allow(dead_code)]
    pub fn config_path() -> PathBuf {
        default_write_path(&RealEnv).unwrap_or_else(|| PathBuf::from("config.toml"))
    }

    pub fn save(&self) -> Result<()> {
        self.save_with_env(&RealEnv)
    }

    pub(crate) fn load_with_env(env: &dyn ConfigEnv) -> Result<Self> {
        let mut config = match resolve_load(env) {
            LoadResolution::EnvOverride(path) => {
                let exists = env.file_exists(&path);
                let mut config = if exists {
                    let contents = std::fs::read_to_string(&path)?;
                    let mut config: Config = toml::from_str(&contents)?;
                    config.expand_tildes();
                    tracing::info!(path = %path.display(), "loaded config (GITPANE_CONFIG)");
                    config
                } else {
                    tracing::info!(
                        path = %path.display(),
                        "GITPANE_CONFIG points to missing file, using defaults"
                    );
                    Config::default()
                };

                config.loaded_path = exists.then(|| path.clone());
                config.write_target_override = Some(path);
                config
            }
            LoadResolution::SearchOrder(paths) => {
                let mut loaded = None;
                for path in &paths {
                    if env.file_exists(path) {
                        let contents = std::fs::read_to_string(path)?;
                        let mut config: Config = toml::from_str(&contents)?;
                        config.expand_tildes();
                        config.loaded_path = Some(path.clone());
                        tracing::info!(path = %path.display(), "loaded config");
                        loaded = Some(config);
                        break;
                    }
                }
                loaded.unwrap_or_else(|| {
                    tracing::info!(candidates = ?paths, "no config file found, using defaults");
                    Config::default()
                })
            }
        };

        config.resolve_theme(env);
        Ok(config)
    }

    pub(crate) fn resolve_theme_with_env(&mut self, env: &dyn ConfigEnv) {
        self.resolve_theme(env);
    }

    /// The theme name actually in effect right now: a CLI `--theme` override
    /// takes precedence over the value persisted in `theme_name`.
    pub fn effective_theme_name(&self) -> &str {
        self.runtime_theme_override
            .as_deref()
            .unwrap_or(&self.theme_name)
    }

    /// Full theme-search list including any dir beside the active config
    /// (`loaded_path` / `write_target_override`). Use this for the in-app
    /// picker and any other code that needs to mirror `resolve_theme`'s
    /// lookup semantics.
    pub(crate) fn theme_dirs(&self, env: &dyn ConfigEnv) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for source in [
            self.loaded_path.as_deref(),
            self.write_target_override.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(parent) = source.parent()
                && !parent.as_os_str().is_empty()
            {
                let parent = parent.to_path_buf();
                if !dirs.contains(&parent) {
                    dirs.push(parent);
                }
            }
        }
        for dir in candidate_theme_dirs(env) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    fn resolve_theme(&mut self, env: &dyn ConfigEnv) {
        let name = self.effective_theme_name().to_string();
        let dirs = self.theme_dirs(env);

        match load_theme(&name, &dirs) {
            Ok(theme) => self.theme = theme,
            Err(e @ LoadThemeError::Unknown { .. }) => {
                tracing::warn!("{e}; falling back to default theme");
                self.theme = Theme::default();
            }
            Err(e @ LoadThemeError::InvalidFile { .. }) => {
                tracing::warn!("{e}; falling back to default theme");
                self.theme = Theme::default();
            }
        }
    }

    pub(crate) fn save_with_env(&self, env: &dyn ConfigEnv) -> Result<()> {
        let config_path = self
            .write_target_override
            .clone()
            .or_else(|| self.loaded_path.clone())
            .or_else(|| default_write_path(env))
            .ok_or_else(|| eyre!("no writable config path available"))?;

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
            if let Some(dir) = &mut self.worktree.dir
                && dir.starts_with("~")
            {
                *dir = home.join(dir.strip_prefix("~").unwrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[derive(Clone, Debug, Default)]
    struct MockEnv {
        gitpane_config: Option<PathBuf>,
        xdg_config_home: Option<PathBuf>,
        home_dir: Option<PathBuf>,
        project_config_dir: Option<PathBuf>,
        existing: HashSet<PathBuf>,
    }

    impl ConfigEnv for MockEnv {
        fn gitpane_config(&self) -> Option<PathBuf> {
            self.gitpane_config
                .clone()
                .filter(|path| !path.as_os_str().is_empty())
        }

        fn xdg_config_home(&self) -> Option<PathBuf> {
            self.xdg_config_home
                .clone()
                .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        }

        fn home_dir(&self) -> Option<PathBuf> {
            self.home_dir.clone()
        }

        fn project_config_dir(&self) -> Option<PathBuf> {
            self.project_config_dir.clone()
        }

        fn file_exists(&self, path: &Path) -> bool {
            self.existing.contains(path)
        }
    }

    fn path(value: &str) -> PathBuf {
        let path = PathBuf::from(value);
        if path.is_absolute() || !value.starts_with('/') {
            path
        } else {
            std::env::current_dir()
                .unwrap()
                .join("mock-root")
                .join(value.trim_start_matches('/'))
        }
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
    fn test_submodule_config_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.submodules.ignore_dirty);
        assert!(config.submodules.warn_unpushed);
    }

    #[test]
    fn test_submodule_config_roundtrip() {
        let mut config = Config::default();
        config.submodules.ignore_dirty = true;
        config.submodules.warn_unpushed = false;
        let serialized = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert!(loaded.submodules.ignore_dirty);
        assert!(!loaded.submodules.warn_unpushed);
    }

    #[test]
    fn test_submodule_config_parse() {
        let toml_str = r#"
            [submodules]
            ignore_dirty = true
            warn_unpushed = false
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.submodules.ignore_dirty);
        assert!(!config.submodules.warn_unpushed);
    }

    #[test]
    fn test_open_config_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.open.command.is_none());
        assert_eq!(config.open.placement, "command");
        // Config::default() is a live runtime fallback, so its placement must
        // match the serde default (not "" from a derived Default).
        assert_eq!(Config::default().open.placement, "command");
    }

    #[test]
    fn test_open_config_roundtrip() {
        let mut config = Config::default();
        config.open.command = Some("cursor {path}".into());
        let serialized = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(loaded.open.command.as_deref(), Some("cursor {path}"));
    }

    #[test]
    fn test_open_config_parse() {
        let toml_str = r#"
            [open]
            command = "tmux new-window -c {path}"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.open.command.as_deref(),
            Some("tmux new-window -c {path}")
        );
    }

    #[test]
    fn test_review_config_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.review.command.is_none());
        assert!(config.review.base.is_none());
        assert_eq!(config.review.placement, "new-window");
        assert_eq!(Config::default().review.placement, "new-window");
    }

    #[test]
    fn test_review_config_roundtrip() {
        let mut config = Config::default();
        config.review.command = Some("git diff {base}...HEAD | delta".into());
        config.review.base = Some("origin/main".into());
        let serialized = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            loaded.review.command.as_deref(),
            Some("git diff {base}...HEAD | delta")
        );
        assert_eq!(loaded.review.base.as_deref(), Some("origin/main"));
    }

    #[test]
    fn test_review_config_parse() {
        let toml_str = r#"
            [review]
            command = "difft"
            base = "develop"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.review.command.as_deref(), Some("difft"));
        assert_eq!(config.review.base.as_deref(), Some("develop"));
    }

    #[test]
    fn test_worktree_config_roundtrip() {
        let mut config = Config::default();
        assert!(config.worktree.dir.is_none());
        config.worktree.dir = Some(PathBuf::from("/wt"));
        let serialized = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(loaded.worktree.dir, Some(PathBuf::from("/wt")));
    }

    #[test]
    fn test_worktree_path_sibling_when_dir_unset() {
        let cfg = WorktreeConfig::default();
        assert_eq!(
            worktree_path(&cfg, Path::new("/home/me/code/app"), "feat/x"),
            PathBuf::from("/home/me/code/app-feat-x")
        );
    }

    #[test]
    fn test_worktree_path_under_configured_dir() {
        let cfg = WorktreeConfig {
            dir: Some(PathBuf::from("/wt")),
        };
        assert_eq!(
            worktree_path(&cfg, Path::new("/home/me/code/app"), "bugfix"),
            PathBuf::from("/wt/app-bugfix")
        );
    }

    #[test]
    fn test_worktree_dir_tilde_expanded() {
        if let Some(home) = dirs::home_dir() {
            let mut config = Config::default();
            config.worktree.dir = Some(PathBuf::from("~/worktrees"));
            config.expand_tildes();
            assert_eq!(config.worktree.dir, Some(home.join("worktrees")));
        }
    }

    #[test]
    fn test_warn_unpushed_defaults_true_when_only_ignore_dirty_set() {
        let toml_str = r#"
            [submodules]
            ignore_dirty = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.submodules.ignore_dirty);
        assert!(config.submodules.warn_unpushed);
    }

    #[test]
    fn test_max_concurrent_polls_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.watch.max_concurrent_polls, 4);
    }

    #[test]
    fn test_refresh_cooldown_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.watch.refresh_cooldown_ms, 5000);
    }

    #[test]
    fn test_watch_worktree_dirs_default() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.watch.watch_worktree_dirs);
    }

    #[test]
    fn test_watch_exclude_dirs_default() {
        let config: Config = toml::from_str("").unwrap();
        assert!(
            config
                .watch
                .watch_exclude_dirs
                .contains(&"node_modules".to_string())
        );
        assert!(
            config
                .watch
                .watch_exclude_dirs
                .contains(&"target".to_string())
        );
        assert!(
            config
                .watch
                .watch_exclude_dirs
                .contains(&".next".to_string())
        );
    }

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
}
