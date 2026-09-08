use super::*;

impl App {
    pub(super) fn refresh_shared_membership(&mut self) -> Result<()> {
        self.last_config_check = Some(Instant::now());
        if self.config.reload_membership()? {
            self.handle_repo_admin(Action::DiscoverNewRepos)?;
            self.action_tx.send(Action::Render)?;
        }
        Ok(())
    }

    pub(super) fn sync_shared_config(&mut self) {
        if let Err(e) = self.refresh_shared_membership() {
            self.error_message = Some((format!("config sync failed: {e}"), Instant::now()));
            let _ = self.action_tx.send(Action::Render);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, App, App) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            root_dirs: vec![],
            write_target_override: Some(dir.path().join("config.toml")),
            ..Config::default()
        };
        config.save().unwrap();
        let first = App::new(config.clone());
        let second = App::new(config);
        (dir, first, second)
    }

    #[tokio::test]
    async fn another_pane_receives_additions_and_removals_on_tick() {
        let (dir, mut first, mut second) = setup();
        let path = dir.path().join("repo");
        git2::Repository::init(&path).unwrap();
        let path = path.canonicalize().unwrap();
        first
            .handle_repo_admin(Action::AddRepo(path.clone()))
            .unwrap();
        second.handle_event(Event::Tick).unwrap();
        assert_eq!(second.repo_list.repos[0].path, path);
        first
            .handle_repo_admin(Action::RemoveRepo(RepoId(path)))
            .unwrap();
        second.last_config_check = None;
        second.handle_event(Event::Tick).unwrap();
        assert!(second.repo_list.repos.is_empty());
        assert!(second.repo_list.selected_repo().is_none());
    }

    #[tokio::test]
    async fn removal_of_discovered_repo_propagates_on_focus() {
        let (dir, mut first, mut second) = setup();
        let path = dir.path().join("repo");
        git2::Repository::init(&path).unwrap();
        let path = path.canonicalize().unwrap();
        for app in [&mut first, &mut second] {
            app.config.root_dirs = vec![dir.path().to_path_buf()];
            app.handle_repo_admin(Action::DiscoverNewRepos).unwrap();
        }
        first
            .handle_repo_admin(Action::RemoveRepo(RepoId(path)))
            .unwrap();
        second.handle_event(Event::FocusGained).unwrap();
        assert!(second.repo_list.repos.is_empty());
        assert!(second.config.excluded_repos.contains(&"repo".to_string()));
    }

    #[tokio::test]
    async fn sleeping_pane_refreshes_shared_membership_on_wake() {
        let (dir, mut first, mut second) = setup();
        let path = dir.path().join("repo");
        git2::Repository::init(&path).unwrap();
        second
            .handle_event(Event::Power(PowerState::DeepSleep))
            .unwrap();
        first
            .handle_repo_admin(Action::AddRepo(path.clone()))
            .unwrap();
        assert!(second.repo_list.repos.is_empty());
        second
            .handle_event(Event::Power(PowerState::Awake))
            .unwrap();
        assert_eq!(second.repo_list.repos[0].path, path.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn add_refreshes_stale_membership_before_saving() {
        let (dir, mut first, mut second) = setup();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        git2::Repository::init(&a).unwrap();
        git2::Repository::init(&b).unwrap();
        first.handle_repo_admin(Action::AddRepo(a.clone())).unwrap();
        second
            .handle_repo_admin(Action::AddRepo(b.clone()))
            .unwrap();
        first.handle_event(Event::FocusGained).unwrap();
        for app in [&first, &second] {
            assert!(
                app.repo_list
                    .repos
                    .iter()
                    .any(|r| r.path == a.canonicalize().unwrap())
            );
            assert!(
                app.repo_list
                    .repos
                    .iter()
                    .any(|r| r.path == b.canonicalize().unwrap())
            );
        }
    }

    #[tokio::test]
    async fn malformed_shared_config_blocks_add_and_surfaces_error() {
        let (dir, mut first, _) = setup();
        let path = dir.path().join("repo");
        git2::Repository::init(&path).unwrap();
        std::fs::write(dir.path().join("config.toml"), "bad = [").unwrap();
        first.handle_repo_admin(Action::AddRepo(path)).unwrap();
        assert!(first.repo_list.repos.is_empty());
        assert!(
            first
                .error_message
                .as_ref()
                .unwrap()
                .0
                .contains("config sync failed")
        );
    }

    #[tokio::test]
    async fn opted_out_pane_keeps_its_view_even_when_saving_changes() {
        for cli_override in [false, true] {
            let (dir, mut first, mut second) = setup();
            if cli_override {
                second.config.runtime_no_sync_repos = true;
            } else {
                second.config.ui.sync_repos = false;
            }
            let a = dir.path().join("a");
            let b = dir.path().join("b");
            git2::Repository::init(&a).unwrap();
            git2::Repository::init(&b).unwrap();
            first.handle_repo_admin(Action::AddRepo(a.clone())).unwrap();
            second.handle_event(Event::Tick).unwrap();
            second.handle_event(Event::FocusGained).unwrap();
            assert!(second.repo_list.repos.is_empty());
            second
                .handle_repo_admin(Action::AddRepo(b.clone()))
                .unwrap();
            assert_eq!(second.repo_list.repos.len(), 1);
            assert_eq!(second.repo_list.repos[0].path, b.canonicalize().unwrap());
            first.handle_event(Event::FocusGained).unwrap();
            assert_eq!(first.repo_list.repos.len(), 2);
        }
    }

    #[tokio::test]
    async fn synchronization_preserves_selected_repo_and_status() {
        let (dir, mut first, mut second) = setup();
        let b = dir.path().join("b");
        let a = dir.path().join("a");
        git2::Repository::init(&a).unwrap();
        git2::Repository::init(&b).unwrap();
        first.handle_repo_admin(Action::AddRepo(b.clone())).unwrap();
        second.handle_event(Event::FocusGained).unwrap();
        second.repo_list.select_repo_row(0);
        std::fs::write(b.join("untracked.txt"), "keep this status").unwrap();
        second.repo_list.repos[0].status =
            Some(crate::git::status::query_status(&b, &second.config.submodules).unwrap());
        first.handle_repo_admin(Action::AddRepo(a)).unwrap();
        second.handle_event(Event::FocusGained).unwrap();
        assert_eq!(
            second.repo_list.selected_repo().unwrap().path,
            b.canonicalize().unwrap()
        );
        assert!(
            second
                .repo_list
                .selected_repo()
                .unwrap()
                .status
                .as_ref()
                .unwrap()
                .is_dirty
        );
    }
}
