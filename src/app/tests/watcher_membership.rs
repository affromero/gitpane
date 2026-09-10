use super::*;

async fn wait_for_watcher(app: &App) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let slot = app.watcher.lock().unwrap();
                if slot
                    .current
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == slot.requested)
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("watcher rebuild did not finish");
}

async fn changed_repo(rx: &mut UnboundedReceiver<Event>, expected: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Event::RepoChanged(path)) = rx.recv().await
                && path == expected
            {
                return;
            }
        }
    })
    .await
    .expect("repository edit did not produce a watcher event");
}

async fn drain_buffered_events(rx: &mut UnboundedReceiver<Event>) {
    // A dropped debouncer can leave already-routed events queued. Wait for a
    // quiet interval before making the edit whose absence we are checking.
    tokio::time::timeout(Duration::from_secs(10), async {
        while tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_ok()
        {}
    })
    .await
    .expect("watcher events did not settle");
}

#[tokio::test]
async fn manually_added_repo_refreshes_in_doze_and_removal_stops_its_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pinned");
    git2::Repository::init(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let mut config = Config {
        root_dirs: vec![],
        write_target_override: Some(dir.path().join("config.toml")),
        ..Config::default()
    };
    config.watch.debounce_ms = 20;
    config.save().unwrap();
    let mut app = App::new(config);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tui_event_tx = Some(tx);
    app.rebuild_watcher();
    wait_for_watcher(&app).await;
    app.handle_repo_admin(Action::AddRepo(path.clone()))
        .unwrap();
    wait_for_watcher(&app).await;
    app.handle_event(Event::Power(PowerState::Doze)).unwrap();
    drain_actions(&mut app);
    while rx.try_recv().is_ok() {}
    std::fs::write(path.join("changed.txt"), "first edit").unwrap();
    changed_repo(&mut rx, &path).await;
    app.handle_event(Event::RepoChanged(path.clone())).unwrap();
    assert!(
        drain_actions(&mut app)
            .iter()
            .any(|action| { matches!(action, Action::RefreshRepo(id) if id.0 == path) }),
        "Doze must refresh repositories after a filesystem edit"
    );

    app.handle_repo_admin(Action::RemoveRepo(RepoId(path.clone())))
        .unwrap();
    wait_for_watcher(&app).await;
    drain_buffered_events(&mut rx).await;
    std::fs::write(path.join("changed.txt"), "edit after removal").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(700), rx.recv())
            .await
            .is_err(),
        "removed repository still emits watcher events"
    );
}

#[tokio::test]
async fn older_watcher_completion_cannot_replace_newer_membership() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old");
    let new_path = dir.path().join("new");
    git2::Repository::init(&old_path).unwrap();
    git2::Repository::init(&new_path).unwrap();
    let old_path = old_path.canonicalize().unwrap();
    let new_path = new_path.canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut slot = WatcherSlot::default();
    let older = slot.request();
    let newer = slot.request();
    let build = |path: &std::path::PathBuf| {
        RepoWatcher::new(std::slice::from_ref(path), &[], 20, tx.clone(), &[], true).unwrap()
    };
    assert!(slot.install(newer, build(&new_path)));
    assert!(!slot.install(older, build(&old_path)));
    while rx.try_recv().is_ok() {}
    std::fs::write(new_path.join("changed.txt"), "new membership").unwrap();
    changed_repo(&mut rx, &new_path).await;
    drain_buffered_events(&mut rx).await;
    std::fs::write(old_path.join("changed.txt"), "stale membership").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(700), async {
            loop {
                if let Some(Event::RepoChanged(path)) = rx.recv().await
                    && path == old_path
                {
                    return;
                }
            }
        })
        .await
        .is_err(),
        "stale watcher remained active"
    );
}
