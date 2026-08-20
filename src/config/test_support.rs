use super::*;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub(super) struct MockEnv {
    pub(super) gitpane_config: Option<PathBuf>,
    pub(super) xdg_config_home: Option<PathBuf>,
    pub(super) home_dir: Option<PathBuf>,
    pub(super) project_config_dir: Option<PathBuf>,
    pub(super) existing: HashSet<PathBuf>,
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

pub(super) fn path(value: &str) -> PathBuf {
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
