use super::*;
use std::{fs, io::Write, path::Path};

impl Config {
    pub(crate) fn sync_repos_enabled(&self) -> bool {
        self.ui.sync_repos && !self.runtime_no_sync_repos
    }

    /// Reload shared repo membership without replacing session settings.
    pub(crate) fn reload_membership(&mut self) -> Result<bool> {
        if !self.sync_repos_enabled() {
            return Ok(false);
        }
        // Constructed configs have no shared source until loaded or assigned
        // an explicit persistence target.
        if self.saved_snapshot.is_none()
            && self.loaded_path.is_none()
            && self.write_target_override.is_none()
        {
            return Ok(false);
        }
        let path = self
            .write_target_override
            .clone()
            .or_else(|| self.loaded_path.clone())
            .or_else(|| default_write_path(&RealEnv))
            .ok_or_else(|| eyre!("no config path available"))?;
        let Some(disk) = read_config(&path)? else {
            return Ok(false);
        };
        let changed =
            self.pinned_repos != disk.pinned_repos || self.excluded_repos != disk.excluded_repos;
        // Only advance the baseline for fields adopted from disk. Other
        // settings remain local, so an unrelated save preserves remote edits.
        if let Some(snapshot) = self.saved_snapshot.as_mut() {
            let value = toml::Value::try_from(&disk)?;
            for key in ["pinned_repos", "excluded_repos"] {
                snapshot[key] = value[key].clone();
            }
        }
        self.pinned_repos = disk.pinned_repos;
        self.excluded_repos = disk.excluded_repos;
        Ok(changed)
    }

    pub(super) fn save_to_path(&mut self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let resolved = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::canonicalize(parent)?.join(
                path.file_name()
                    .ok_or_else(|| eyre!("invalid config path"))?,
            ),
            Err(e) => return Err(e.into()),
        };
        let path = resolved.as_path();
        let parent = path
            .parent()
            .ok_or_else(|| eyre!("invalid config parent"))?;
        // Keep the lock inode stable while the config itself is replaced.
        let mut lock_name = path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_name)?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let local = toml::Value::try_from(&*self)?;
        let mut merged = match read_config(path)? {
            Some(disk) => toml::Value::try_from(disk)?,
            None => local.clone(),
        };
        if let Some(base) = &self.saved_snapshot {
            merge_changes(base, &local, &mut merged);
        } else {
            merged = local;
        }
        let adopted: Config = merged.clone().try_into()?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        if let Ok(metadata) = fs::metadata(path) {
            temp.as_file().set_permissions(metadata.permissions())?;
        }
        temp.write_all(toml::to_string_pretty(&merged)?.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        if self.sync_repos_enabled() {
            self.pinned_repos = adopted.pinned_repos;
            self.excluded_repos = adopted.excluded_repos;
        }
        self.saved_snapshot = Some(toml::Value::try_from(&*self)?);
        Ok(())
    }
}

fn read_config(path: &Path) -> Result<Option<Config>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut config: Config = toml::from_str(&contents)?;
    config.expand_tildes();
    Ok(Some(config))
}

fn merge_changes(base: &toml::Value, local: &toml::Value, disk: &mut toml::Value) {
    if base == local {
        return;
    }
    if let (Some(base), Some(local), Some(disk)) =
        (base.as_table(), local.as_table(), disk.as_table_mut())
    {
        for key in base.keys().filter(|key| !local.contains_key(*key)) {
            disk.remove(key);
        }
        for (key, value) in local {
            if base.get(key) == Some(value) {
                continue;
            }
            match (base.get(key), disk.get_mut(key)) {
                (Some(old), Some(current)) if key == "pinned_repos" || key == "excluded_repos" => {
                    if let (Some(old), Some(new), Some(current)) =
                        (old.as_array(), value.as_array(), current.as_array_mut())
                    {
                        current.retain(|v| !old.contains(v) || new.contains(v));
                        for item in new {
                            if !old.contains(item) && !current.contains(item) {
                                current.push(item.clone());
                            }
                        }
                    }
                }
                (Some(old), Some(current)) => merge_changes(old, value, current),
                _ => {
                    disk.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *disk = local.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(path: &Path) -> Config {
        Config::load_with_env(&super::super::test_support::MockEnv {
            gitpane_config: Some(path.to_path_buf()),
            existing: [path.to_path_buf()].into(),
            ..Default::default()
        })
        .unwrap()
    }

    fn setup() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "root_dirs = []\npinned_repos = ['/tmp/old']\n").unwrap();
        let config = load(&path);
        (dir, config)
    }

    #[test]
    fn stale_saves_preserve_removals_additions_and_other_settings() {
        let (dir, mut first) = setup();
        let path = dir.path().join("config.toml");
        let mut second = load(&path);
        first.pinned_repos.clear();
        first.theme_name = "muted".into();
        first.save().unwrap();
        second.pinned_repos.push("/tmp/new".into());
        second.save().unwrap();
        let disk = load(&path);
        assert_eq!(disk.pinned_repos, vec![PathBuf::from("/tmp/new")]);
        assert_eq!(disk.theme_name, "muted");
        second.scan_depth = 7;
        second.save().unwrap();
        assert_eq!(load(&path).theme_name, "muted");
        assert_eq!(load(&path).scan_depth, 7);
    }

    #[test]
    fn concurrent_saves_preserve_independent_additions() {
        let (dir, first) = setup();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let mut config = first.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    config.pinned_repos.push(format!("/tmp/repo-{i}").into());
                    barrier.wait();
                    config.save().unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let disk = load(&dir.path().join("config.toml"));
        for i in 0..4 {
            assert!(disk.pinned_repos.contains(&format!("/tmp/repo-{i}").into()));
        }
        assert!(disk.pinned_repos.contains(&PathBuf::from("/tmp/old")));
    }

    #[test]
    fn optional_settings_can_be_cleared_without_stale_writers_restoring_them() {
        let (dir, mut first) = setup();
        let path = dir.path().join("config.toml");
        first.open.command = Some("editor {path}".into());
        first.save().unwrap();
        let mut second = load(&path);
        first.open.command = None;
        first.save().unwrap();
        assert!(load(&path).open.command.is_none());
        second.open.placement = "new-window".into();
        second.save().unwrap();
        let disk = load(&path);
        assert!(disk.open.command.is_none());
        assert_eq!(disk.open.placement, "new-window");
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_preserves_the_link_and_shared_target() {
        let (dir, mut first) = setup();
        let path = dir.path().join("config.toml");
        let alias = dir.path().join("alias.toml");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let mut second = load(&alias);
        first.pinned_repos.clear();
        first.save().unwrap();
        second.pinned_repos.push("/tmp/new".into());
        second.save().unwrap();
        assert!(alias.is_symlink());
        assert_eq!(load(&path).pinned_repos, vec![PathBuf::from("/tmp/new")]);
    }

    #[test]
    fn reload_preserves_runtime_overrides_and_remote_settings_on_save() {
        let (dir, mut first) = setup();
        let path = dir.path().join("config.toml");
        let mut second = load(&path);
        second.runtime_root_override = Some("/tmp/session".into());
        second.runtime_theme_override = Some("muted".into());
        first.pinned_repos.clear();
        first.theme_name = "muted".into();
        first.save().unwrap();
        assert!(second.reload_membership().unwrap());
        assert_eq!(second.runtime_root_override, Some("/tmp/session".into()));
        assert_eq!(second.runtime_theme_override.as_deref(), Some("muted"));
        second.save().unwrap();
        let disk = load(&path);
        assert!(disk.pinned_repos.is_empty());
        assert!(disk.root_dirs.is_empty());
        assert_eq!(disk.theme_name, "muted");
    }

    #[test]
    fn invalid_config_is_reported_and_never_overwritten() {
        let (dir, mut config) = setup();
        let path = dir.path().join("config.toml");
        fs::write(&path, "invalid = [").unwrap();
        assert!(config.reload_membership().is_err());
        assert!(config.save().is_err());
        assert_eq!(config.pinned_repos, vec![PathBuf::from("/tmp/old")]);
        assert_eq!(fs::read_to_string(path).unwrap(), "invalid = [");
    }

    #[test]
    fn missing_config_can_be_created_and_then_shared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new/config.toml");
        let mut first = Config::load_with_env(&super::super::test_support::MockEnv {
            gitpane_config: Some(path.clone()),
            ..Default::default()
        })
        .unwrap();
        let mut second = first.clone();
        assert!(!second.reload_membership().unwrap());
        first.pinned_repos.push("/tmp/new".into());
        first.save().unwrap();
        assert!(second.reload_membership().unwrap());
        assert_eq!(second.pinned_repos, vec![PathBuf::from("/tmp/new")]);
    }
}
