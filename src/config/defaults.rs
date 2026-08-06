use super::terminal::default_goto_command;
use super::*;

impl Default for SubmoduleConfig {
    fn default() -> Self {
        Self {
            ignore_dirty: false,
            warn_unpushed: default_warn_unpushed(),
        }
    }
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            enabled: default_github_enabled(),
        }
    }
}

pub(super) fn default_github_enabled() -> bool {
    true
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            command: None,
            placement: default_open_placement(),
        }
    }
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

pub(super) fn default_open_placement() -> String {
    "command".to_string()
}

pub(super) fn default_review_placement() -> String {
    "new-window".to_string()
}

impl Default for GotoConfig {
    fn default() -> Self {
        Self {
            command: default_goto_command(),
        }
    }
}

pub(super) fn default_warn_unpushed() -> bool {
    true
}

pub(super) fn default_show_stats() -> bool {
    true
}

pub(super) fn default_label_max_len() -> usize {
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

pub(super) fn default_root_dirs() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|h| vec![h.join("Code")])
        .unwrap_or_default()
}

pub(super) fn default_scan_depth() -> usize {
    2
}

pub(super) fn default_theme_name() -> String {
    "default".into()
}

pub(super) fn default_debounce_ms() -> u64 {
    500
}

pub(super) fn default_refresh_cooldown_ms() -> u64 {
    5000
}

pub(super) fn default_watch_worktree_dirs() -> bool {
    false
}

pub(super) fn default_discovery_cooldown_secs() -> u64 {
    5
}

pub(super) fn default_poll_local_secs() -> u64 {
    5
}

pub(super) fn default_poll_fetch_secs() -> u64 {
    30
}

pub(super) fn default_max_concurrent_polls() -> usize {
    4
}

pub(super) fn default_watch_exclude_dirs() -> Vec<String> {
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

pub(super) fn default_frame_rate() -> u16 {
    10
}

pub(super) fn default_show_liveness() -> bool {
    true
}

pub(super) fn default_check_for_updates() -> bool {
    true
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
            github: GithubConfig::default(),
            open: OpenConfig::default(),
            review: ReviewConfig::default(),
            worktree: WorktreeConfig::default(),
            goto: GotoConfig::default(),
            keybindings: Vec::new(),
            theme_name: default_theme_name(),
            theme: Theme::default(),
            runtime_theme_override: None,
            loaded_path: None,
            write_target_override: None,
        }
    }
}
