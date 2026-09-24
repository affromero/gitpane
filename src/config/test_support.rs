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
        self.gitpane_config.clone()
    }

    fn xdg_config_home(&self) -> Option<PathBuf> {
        self.xdg_config_home.clone()
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

/// Run environment-sensitive assertions in a child without changing the
/// environment of other tests in this process. Returns true in that child.
pub(super) fn in_config_env(test: &str, values: &[(&str, &std::ffi::OsStr)]) -> bool {
    const CHILD: &str = "GITPANE_TEST_CONFIG_ENV_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env_remove("GITPANE_CONFIG")
        .env_remove("XDG_CONFIG_HOME")
        .env(CHILD, test)
        .envs(values.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{test}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    false
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
