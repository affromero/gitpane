use color_eyre::{Result, eyre::eyre};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::theme::{LoadThemeError, Theme, load_theme};

mod defaults;
mod load;
mod terminal;
#[cfg(test)]
mod tests;

use defaults::*;
use load::{LoadResolution, candidate_search_paths, default_write_path, resolve_load};
use terminal::default_goto_command;

pub(crate) use load::{ConfigEnv, RealEnv, candidate_theme_dirs, worktree_path};

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
    pub github: GithubConfig,
    #[serde(default)]
    pub open: OpenConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub worktree: WorktreeConfig,
    #[serde(default)]
    pub goto: GotoConfig,
    /// User-defined keybindings. Each binds a single key to a templated shell
    /// command run against the selected repo/worktree. See [`Keybinding`].
    #[serde(default)]
    pub keybindings: Vec<Keybinding>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GithubConfig {
    /// Show the GitHub panel (open issues/PRs for the selected repo, fetched via
    /// the `gh` CLI). Opt-out: enabled by default. The panel only appears for a
    /// selected repo whose `origin` is a github.com remote and only when it has
    /// open issues or PRs (press `p` to force it open otherwise). Requires `gh`
    /// on PATH; absent, gitpane keeps the usual three panels with no error.
    #[serde(default = "default_github_enabled")]
    pub enabled: bool,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GotoConfig {
    /// Command run by `G` to open a repo's live tmux session, `{session}` being
    /// the session name. By default it is auto-detected from your terminal (see
    /// the `TERMINAL_GOTOS` table) to open a new tab/window. Override here for an
    /// unsupported terminal or a different behavior, e.g.
    /// `wezterm cli spawn -- tmux attach -t {session}`. Run as argv (no shell),
    /// `{session}` substituted per token.
    #[serde(default = "default_goto_command")]
    pub command: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct WorktreeConfig {
    /// Directory new worktrees are created under, each as `<repo>-<branch>`.
    /// When unset, a worktree is created as a sibling of its repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// One user-defined keybinding: press `key` to run `command` against the
/// selected repo/worktree. Configured as a `[[keybindings]]` array in
/// `config.toml`, e.g.
///
/// ```toml
/// [[keybindings]]
/// key = "b"
/// command = "gh repo view --web"
/// placement = "command"
/// desc = "Open repo on github.com"
/// ```
///
/// `key` is a single character. Keys already bound to built-in actions
/// (`o v G r R t g p q y a d s`, `Tab`, `Esc`, `?`) are reserved and cannot be
/// rebound; a panel-local key (`j`/`k`/`Enter`/…) can be shadowed but then no
/// longer navigates that panel. `command` uses the same `{path}` substitution
/// and `placement` vocabulary as `[open]` (`command`/`inline`/`ask`/
/// `split-window`/`new-window`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Keybinding {
    /// The single character that triggers the command.
    pub key: String,
    /// Command template. `{path}` expands to the target directory.
    pub command: String,
    /// Where/how to launch, like `[open] placement`. Defaults to `command`
    /// (run the command directly as its own launcher).
    #[serde(default = "default_open_placement")]
    pub placement: String,
    /// Optional label shown in the help overlay; falls back to the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
}

/// True when a keybinding's `key` is exactly the single character `c`. A
/// multi-char `key` (e.g. `"ctrl+b"`) never matches, so it is inert rather
/// than firing on `c`.
pub(crate) fn key_matches(key: &str, c: char) -> bool {
    key.chars().eq(std::iter::once(c))
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

    /// Config files that exist on disk but are ignored because another file
    /// won the first-match search (or a `GITPANE_CONFIG` override). Loading
    /// never merges, so settings in these files silently do nothing — surface
    /// them in diagnostics instead of letting pinned repos "vanish".
    pub(crate) fn shadowed_config_paths(&self, env: &dyn ConfigEnv) -> Vec<PathBuf> {
        candidate_search_paths(env)
            .into_iter()
            .filter(|p| env.file_exists(p) && Some(p.as_path()) != self.loaded_path.as_deref())
            .collect()
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
