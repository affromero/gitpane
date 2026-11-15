use super::*;
use crate::git::status::{SubmoduleInfo, SubmoduleWarn, WorktreeEntry};
use ratatui::{Terminal, backend::TestBackend};

fn rows(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal
        .draw(|frame| app.repo_list.draw(frame, frame.area()).unwrap())
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[tokio::test]
async fn removing_and_adding_a_repo_resets_its_submodule_disclosures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repo");
    git2::Repository::init(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let config = Config {
        root_dirs: vec![],
        pinned_repos: vec![path.clone()],
        write_target_override: Some(dir.path().join("config.toml")),
        ..Config::default()
    };
    let mut app = App::new(config).unwrap();
    let sub = |name: &str| SubmoduleInfo {
        name: name.into(),
        path: name.into(),
        state: None,
        head: None,
        head_oid: None,
        workdir_oid: None,
        warn: SubmoduleWarn::default(),
    };
    let mut status = test_status("main", None);
    status.checkout_submodules.push(sub("main-child"));
    status.worktree_info.push(WorktreeEntry {
        name: "linked".into(),
        path: dir.path().join("linked"),
        branch: "linked".into(),
        ahead: 0,
        behind: 0,
        is_dirty: false,
        file_count: 0,
        has_dirty_submodules: false,
        has_unpushed_submodules: false,
        checkout_submodules: vec![sub("worktree-child")],
    });
    app.repo_list.update_status(0, status.clone());
    app.repo_list
        .handle_key_event(KeyCode::Char('m').into())
        .unwrap();
    app.repo_list
        .handle_key_event(KeyCode::Down.into())
        .unwrap();
    app.repo_list
        .handle_key_event(KeyCode::Char('m').into())
        .unwrap();
    assert!(!rows(&mut app).contains("main-child"));
    assert!(!rows(&mut app).contains("worktree-child"));
    app.handle_repo_admin(Action::RemoveRepo(RepoId(path.clone())))
        .unwrap();
    app.handle_repo_admin(Action::AddRepo(path)).unwrap();
    app.repo_list.update_status(0, status);
    let rendered = rows(&mut app);
    assert!(rendered.contains("main-child"));
    assert!(rendered.contains("worktree-child"));
}
