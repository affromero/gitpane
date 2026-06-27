use directories::ProjectDirs;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use super::{APP_NAME, CONFIG_FILE, WorktreeConfig};

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
pub(super) enum LoadResolution {
    EnvOverride(PathBuf),
    SearchOrder(Vec<PathBuf>),
}

pub(super) fn resolve_load(env: &dyn ConfigEnv) -> LoadResolution {
    if let Some(path) = env.gitpane_config() {
        return LoadResolution::EnvOverride(path);
    }

    LoadResolution::SearchOrder(candidate_search_paths(env))
}

pub(super) fn xdg_config_path(config_home: PathBuf) -> PathBuf {
    config_home.join(APP_NAME).join(CONFIG_FILE)
}

pub(super) fn dot_config_path(home: PathBuf) -> PathBuf {
    home.join(".config").join(APP_NAME).join(CONFIG_FILE)
}

pub(super) fn candidate_search_paths(env: &dyn ConfigEnv) -> Vec<PathBuf> {
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

pub(super) fn default_write_path(env: &dyn ConfigEnv) -> Option<PathBuf> {
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
