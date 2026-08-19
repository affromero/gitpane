use super::defaults::default_scan_depth;
use super::load::{LoadResolution, candidate_search_paths, default_write_path, resolve_load};
use super::terminal::{TERMINAL_GOTOS, goto_command_for_env};
use super::*;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

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

mod fields;
mod paths;
mod theme;
