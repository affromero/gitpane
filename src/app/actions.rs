use super::*;

impl App {
    pub(super) fn handle_action(&mut self, action: Action, tui: &mut Tui) -> Result<()> {
        if self.file_list.is_stash() && action.is_workspace_file_action() {
            self.action_tx.send(Action::Error(
                "Stash preview is read-only; return to the working tree to change files"
                    .to_string(),
            ))?;
            return Ok(());
        }
        match action {
            Action::Quit => {
                if self.should_quit {
                    // Second request: stop waiting for in-flight git ops.
                    self.force_quit = true;
                } else {
                    self.should_quit = true;
                    if MUTATING_GIT_OPS.load(Ordering::SeqCst) > 0 {
                        self.action_tx.send(Action::Error(
                            "waiting for a git operation to finish; press q again to quit now"
                                .to_string(),
                        ))?;
                    }
                }
            }
            Action::Tick => {
                if self.clear_expired_messages() {
                    self.action_tx.send(Action::Render)?;
                }
            }
            Action::Render => {
                tui.terminal.draw(|frame| {
                    let _ = self.draw(frame);
                })?;
            }
            Action::Resize(w, h) => {
                tui.terminal
                    .resize(ratatui::layout::Rect::new(0, 0, w, h))?;
            }
            Action::CopyText(ref text) => {
                self.copy_to_clipboard(text);
            }
            Action::SelectRepo(ref id) => {
                self.context_menu.hide();
                self.active_worktree = None;
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    let entry = &self.repo_list.repos[idx];
                    let name = entry.name.clone();
                    let path = entry.path.clone();
                    let repo_id = id.clone();
                    let files = entry
                        .status
                        .as_ref()
                        .map(|s| s.files.clone())
                        .unwrap_or_default();
                    self.file_list.set_files(files, &name, repo_id);
                    self.git_graph.load_repo(path, &name);
                    self.repo_list.select_repo_row(idx);
                }
                self.github_touch_selection();
            }
            Action::SelectStash {
                repo_id,
                oid,
                label,
                summary,
            } => {
                self.context_menu.hide();
                self.active_worktree = None;
                if let Some(idx) = self.repo_list.resolve_index(&repo_id) {
                    let entry = &self.repo_list.repos[idx];
                    let name = entry.name.clone();
                    let path = entry.path.clone();
                    let generation = self.file_list.show_stash_loading(
                        repo_id.clone(),
                        name.clone(),
                        oid.clone(),
                        label,
                        summary,
                    );
                    self.git_graph.load_repo(path.clone(), &name);
                    let tx = self.action_tx.clone();
                    let load_oid = oid.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = super::stash::load_stash_files(&path, &load_oid)
                            .map_err(|error| error.to_string());
                        let _ = tx.send(Action::StashFilesLoaded {
                            repo_id,
                            oid: load_oid,
                            generation,
                            result,
                        });
                    });
                }
                self.github_touch_selection();
            }
            Action::ExitStashView => {
                self.exit_stash_view()?;
            }
            Action::SelectWorktree {
                ref repo_id,
                ref worktree_path,
                ref worktree_branch,
            } => {
                let repo_name = self
                    .repo_list
                    .resolve_index(repo_id)
                    .map(|i| self.repo_list.repos[i].name.clone())
                    .unwrap_or_default();
                let display_name = format!("{}:{}", repo_name, worktree_branch);
                self.activate_path_context(worktree_path.clone(), repo_id.clone(), display_name);
                self.github_touch_selection();
            }
            Action::SelectSubmodule {
                ref repo_id,
                ref sub_path,
            } => self.select_submodule(repo_id, sub_path)?,
            Action::WorktreeFilesLoaded {
                ref repo_id,
                ref worktree_path,
                ref name,
                ref files,
                ref submodules,
            } => {
                // Only apply if this worktree is still selected
                if self
                    .active_worktree
                    .as_ref()
                    .is_some_and(|aw| aw.path == *worktree_path && aw.repo_id == *repo_id)
                {
                    if let Some(active) = self.active_worktree.as_mut() {
                        active.submodules.clone_from(submodules);
                    }
                    self.file_list
                        .set_files(files.clone(), name, repo_id.clone());
                    if let Some(action) = self.file_list.refresh_diff() {
                        self.action_tx.send(action)?;
                    }
                }
            }
            Action::StatusQueryDone(ref id) => {
                self.pending_status.remove(id);
                self.last_refresh.insert(id.clone(), Instant::now());
                // Clear git_op so the repo isn't permanently skipped
                // by future polls after a failed status query.
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    self.repo_list.repos[idx].git_op = false;
                }
                if self.dirty_repos.remove(id) {
                    self.schedule_refresh(id);
                }
            }
            Action::RepoStatusUpdated { ref id, ref status } => {
                self.pending_status.remove(id);
                self.last_refresh.insert(id.clone(), Instant::now());
                let is_dirty = self.dirty_repos.remove(id);
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    let graph_changed = graph_status_changed(
                        self.repo_list.repos[idx].status.as_ref(),
                        status,
                        self.git_graph.graph_options.branch_filter,
                    );
                    let selection = self.repo_list.selected_row_id();
                    let status_clone = status.clone();
                    self.repo_list.update_status(idx, status_clone);
                    if matches!(
                        selection,
                        Some(crate::components::repo_list::RowId::Submodule(_, _))
                    ) && selection != self.repo_list.selected_row_id()
                        && let Some(action) = self.repo_list.emit_selection_action()
                    {
                        self.action_tx.send(action)?;
                    }
                    // A stash preview follows its OID: renumbering refreshes
                    // the label, deletion falls back to the working tree.
                    self.reconcile_stash_preview();

                    // A graph-relevant change invalidates the cached snapshot
                    // even when this repo isn't selected, so switching to it
                    // later rebuilds from disk instead of resurrecting stale
                    // rows.
                    if graph_changed {
                        self.git_graph
                            .invalidate_repo(&self.repo_list.repos[idx].path);
                    }

                    // Skip when a worktree
                    // is being viewed — its files come from WorktreeFilesLoaded,
                    // not the parent repo's status.
                    if self.repo_list.selected_index() == Some(idx)
                        && self.active_worktree.is_none()
                        && !self.file_list.is_stash()
                    {
                        let entry = &self.repo_list.repos[idx];
                        let name = entry.name.clone();
                        let repo_id = id.clone();
                        let files = entry
                            .status
                            .as_ref()
                            .map(|s| s.files.clone())
                            .unwrap_or_default();
                        self.file_list.set_files(files, &name, repo_id);
                        if let Some(action) = self.file_list.refresh_diff() {
                            self.action_tx.send(action)?;
                        }

                        if graph_changed {
                            if self.git_graph.has_detail() {
                                self.git_graph.set_needs_reload();
                            } else {
                                let path = entry.path.clone();
                                self.git_graph.load_repo(path, &name);
                            }
                        }
                    }
                }
                if is_dirty {
                    self.schedule_refresh(id);
                }
            }
            Action::RefreshAll => {
                // User-initiated refresh: fetch from remote + show spinner
                let sub_cfg = self.config.submodules.clone();
                for entry in self.repo_list.repos.iter_mut() {
                    entry.git_op = true;
                    let repo_id = RepoId(entry.path.clone());
                    self.pending_status.insert(repo_id.clone());
                    let path = entry.path.clone();
                    let tx = self.action_tx.clone();
                    let sem = self.poll_semaphore.clone();
                    let sub_cfg = sub_cfg.clone();
                    tokio::spawn(async move {
                        let _permit = sem.acquire().await;
                        let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                        tokio::task::spawn_blocking(move || {
                            match crate::git::status::query_status_with_fetch(&path, &sub_cfg) {
                                Ok(s) => {
                                    let _ = tx.send(Action::RepoStatusUpdated {
                                        id: repo_id.clone(),
                                        status: s,
                                    });
                                    guard.complete();
                                }
                                Err(e) => {
                                    guard.complete();
                                    let _ = tx.send(Action::StatusQueryDone(repo_id));
                                    let _ =
                                        tx.send(Action::Error(format!("Failed to query: {}", e)));
                                }
                            }
                        })
                        .await
                    });
                }
            }
            Action::PollLocal => self.poll_local(),
            Action::PollFetch => {
                // Remote fetch poll (updates ahead/behind, no spinner)
                let sub_cfg = self.config.submodules.clone();
                for entry in self.repo_list.repos.iter() {
                    let repo_id = RepoId(entry.path.clone());
                    if entry.git_op || self.pending_status.contains(&repo_id) {
                        continue;
                    }
                    self.pending_status.insert(repo_id.clone());
                    let path = entry.path.clone();
                    let tx = self.action_tx.clone();
                    let sem = self.poll_semaphore.clone();
                    let sub_cfg = sub_cfg.clone();
                    tokio::spawn(async move {
                        let _permit = sem.acquire().await;
                        let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                        tokio::task::spawn_blocking(move || {
                            match crate::git::status::query_status_with_fetch(&path, &sub_cfg) {
                                Ok(s) => {
                                    let _ = tx.send(Action::RepoStatusUpdated {
                                        id: repo_id.clone(),
                                        status: s,
                                    });
                                    guard.complete();
                                }
                                Err(e) => {
                                    guard.complete();
                                    let _ = tx.send(Action::StatusQueryDone(repo_id));
                                    tracing::debug!(
                                        "Fetch poll failed for {}: {}",
                                        path.display(),
                                        e
                                    );
                                }
                            }
                        })
                        .await
                    });
                }
            }
            Action::RefreshRepo(ref id) => {
                // Watcher-triggered: fast local-only, no spinner. A
                // worktree path resolves to its parent repo, whose
                // status query re-reads worktree state.
                let parent_id = self
                    .repo_list
                    .resolve_target(id)
                    .map(|t| RepoId(self.repo_list.repos[t.parent_index].path.clone()));
                self.schedule_refresh(parent_id.as_ref().unwrap_or(id));
            }
            Action::RefreshRepoAfterCooldown(ref id) => {
                if self.refresh_scheduled.remove(id) {
                    self.schedule_refresh(id);
                }
            }
            Action::ShowGitGraph => {
                // Force-reload the graph for the current selection. A
                // worktree row routes through SelectWorktree so the
                // graph and changes panel target the worktree, matching
                // the other context-menu items; a repo row loads the
                // repo's own graph.
                self.context_menu.hide();
                if self.repo_list.selected_submodule_uninitialized() {
                    self.action_tx
                        .send(Action::Error("Submodule is not initialized".to_string()))?;
                    return Ok(());
                }
                // A submodule/worktree context owns the graph: refresh from its
                // path rather than reloading the selected parent repo over it.
                if self.active_worktree.is_some() {
                    self.refresh_active_worktree();
                } else {
                    let worktree = self
                        .repo_list
                        .selected_worktree()
                        .map(|(repo_id, wt)| (repo_id, wt.path.clone(), wt.branch.clone()));
                    if let Some((repo_id, worktree_path, worktree_branch)) = worktree {
                        self.action_tx.send(Action::SelectWorktree {
                            repo_id,
                            worktree_path,
                            worktree_branch,
                        })?;
                    } else if let Some(entry) = self.repo_list.selected_repo() {
                        let path = entry.path.clone();
                        let name = entry.name.clone();
                        // `g` is an explicit reload: bypass the cache so a
                        // fresh on-disk graph is drawn even if nothing changed.
                        self.git_graph.force_reload_repo(path, &name);
                    }
                }
                self.focus = FocusPanel::Graph;
            }
            Action::OpenSelected => {
                if let Some(path) = self.selected_launch_path() {
                    self.launch_open(path, tui)?;
                }
            }
            Action::OpenAt(ref id) => {
                if let Some(path) = self.repo_list.resolve_target(id).map(|t| t.exec_path) {
                    self.launch_open(path, tui)?;
                }
            }
            Action::ReviewSelected => {
                if let Some(path) = self.selected_launch_path() {
                    self.launch_review(path, tui)?;
                }
            }
            Action::ReviewAt(ref id) => {
                if let Some(path) = self.repo_list.resolve_target(id).map(|t| t.exec_path) {
                    self.launch_review(path, tui)?;
                }
            }
            Action::RunKeybinding(idx) => {
                if let Some(path) = self.selected_launch_path() {
                    self.launch_keybinding(idx, path, tui)?;
                }
            }
            Action::OpenNewWorktree(ref repo_id) => {
                self.path_input.show_new_worktree(repo_id.clone());
            }
            Action::CreateWorktree {
                ref repo,
                ref branch,
            } => {
                // A leading '-' would be read as an option by git; reject
                // it up front with a clear message rather than a cryptic
                // git error.
                if branch.starts_with('-') {
                    self.action_tx
                        .send(Action::Error("branch name cannot start with '-'".into()))?;
                } else {
                    let new_path =
                        crate::config::worktree_path(&self.config.worktree, repo, branch);
                    // `-b <branch>` then `--` so the path is never parsed
                    // as an option.
                    let args = crate::git::worktree_ops::create_arguments(branch, &new_path);
                    // The new row arrives with the next status poll; make
                    // sure a collapsed repo is showing by the time it does.
                    self.repo_list.expand_repo(repo);
                    self.spawn_repo_git_op(repo.clone(), args);
                }
            }
            Action::RemoveWorktreeSelected => {
                // `d` key: resolve the selected worktree, then confirm.
                let wt = self
                    .repo_list
                    .selected_worktree()
                    .map(|(rid, wt)| (rid.0.clone(), wt.path.clone(), wt.branch.clone()));
                if let Some((repo, worktree_path, branch)) = wt {
                    self.confirm_remove_worktree(repo, worktree_path, branch)?;
                }
            }
            Action::RemoveWorktreeAt(ref id) => {
                // Context menu: resolve the right-clicked worktree by id so
                // a row re-sort can't retarget a different worktree.
                if let Some((repo, worktree_path, branch)) =
                    self.repo_list.worktree_remove_target(id)
                {
                    self.confirm_remove_worktree(repo, worktree_path, branch)?;
                }
            }
            Action::RemoveWorktree {
                ref repo,
                ref worktree_path,
            } => {
                // If we're removing the worktree whose changes/graph are
                // currently shown, drop it so nothing refreshes against a
                // now-deleted path.
                if self
                    .active_worktree
                    .as_ref()
                    .is_some_and(|aw| &aw.path == worktree_path)
                {
                    self.active_worktree = None;
                }
                let args = crate::git::worktree_ops::remove_arguments(worktree_path);
                self.spawn_repo_git_op(repo.clone(), args);
            }
            Action::LiveSessionsLoaded(result) => {
                self.complete_liveness_probe(result);
            }
            Action::PlacementChoicesLoaded { generation, result } => {
                self.finish_placement_picker(generation, result);
            }
            Action::PickerChose(ref value) => {
                self.picker.hide();
                match self.pending_pick.take() {
                    Some(PendingPick::Launch(p)) => {
                        let plan = crate::session::launcher::plan_with_target(
                            p.command.as_deref(),
                            value,
                            &p.dir.to_string_lossy(),
                            &p.target.to_string_lossy(),
                            p.base.as_deref(),
                            self.mux,
                        );
                        self.run_launch_plan(plan, p.dir, p.label, tui)?;
                    }
                    Some(PendingPick::GotoSession) => self.goto_session(value),
                    None => {}
                }
            }
            Action::PickerCancel => {
                self.picker.hide();
                self.pending_pick = None;
            }
            Action::OpenFile(ref id, ref path) => {
                // Lives here (not handle_action_rest) because launch_open needs
                // the Tui to suspend/restore for an inline editor.
                self.open_file_external(id, path, tui)?;
            }
            other => self.handle_action_rest(other)?,
        }
        Ok(())
    }

    pub(super) fn complete_liveness_probe(
        &mut self,
        result: Result<Vec<(String, std::path::PathBuf)>, String>,
    ) {
        self.liveness_probe_in_flight = false;
        match result {
            Ok(panes) => {
                self.liveness_error = None;
                self.repo_list.set_live_panes(panes);
            }
            Err(error) => {
                self.repo_list.set_live_panes(Vec::new());
                if self.liveness_error.is_none() {
                    let _ = self.action_tx.send(Action::Error(format!(
                        "Herdr desktop liveness failed: {error}"
                    )));
                }
                self.liveness_error = Some(error);
            }
        }
    }

    pub(super) fn poll_local(&mut self) {
        // Probe live panes once per poll so repos/worktrees get a
        // marker. One `tmux list-panes` (or `herdr pane list`) call,
        // off-thread; empty set otherwise.
        if self.config.ui.show_liveness && !self.liveness_probe_in_flight {
            self.liveness_probe_in_flight = true;
            let tx = self.action_tx.clone();
            let mux = self.mux;
            tokio::task::spawn_blocking(move || {
                let panes = match mux {
                    crate::session::env::Multiplexer::Tmux => {
                        Ok(crate::session::liveness::tmux_pane_sessions())
                    }
                    crate::session::env::Multiplexer::Herdr => {
                        Ok(crate::session::liveness::herdr_live_panes())
                    }
                    crate::session::env::Multiplexer::HerdrApp => {
                        crate::session::liveness::herdr_app_live_panes()
                    }
                    crate::session::env::Multiplexer::None => Ok(Vec::new()),
                };
                let _ = tx.send(Action::LiveSessionsLoaded(panes));
            });
        }
        // Fast local status poll (no network, no spinner)
        let sub_cfg = self.config.submodules.clone();
        for entry in self.repo_list.repos.iter() {
            let repo_id = RepoId(entry.path.clone());
            if entry.git_op || self.pending_status.contains(&repo_id) {
                continue;
            }
            self.pending_status.insert(repo_id.clone());
            let path = entry.path.clone();
            let tx = self.action_tx.clone();
            let sem = self.poll_semaphore.clone();
            let sub_cfg = sub_cfg.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let guard = StatusGuard::new(repo_id.clone(), tx.clone());
                tokio::task::spawn_blocking(move || {
                    match crate::git::status::query_status(&path, &sub_cfg) {
                        Ok(s) => {
                            let _ = tx.send(Action::RepoStatusUpdated {
                                id: repo_id.clone(),
                                status: s,
                            });
                            guard.complete();
                        }
                        Err(e) => {
                            guard.complete();
                            let _ = tx.send(Action::StatusQueryDone(repo_id));
                            tracing::debug!("Local poll failed for {}: {}", path.display(), e);
                            if !crate::git::scanner::is_repo_root(&path) {
                                let _ = tx.send(Action::DiscoverNewRepos);
                            }
                        }
                    }
                })
                .await
            });
        }

        // Also re-query the active worktree so its changes update live
        self.refresh_active_worktree();
    }

    fn select_submodule(&mut self, repo_id: &RepoId, sub_path: &std::path::Path) -> Result<()> {
        let Some(idx) = self.repo_list.resolve_index(repo_id) else {
            return Ok(());
        };
        let entry = &self.repo_list.repos[idx];
        let active = self
            .active_worktree
            .as_ref()
            .filter(|aw| aw.repo_id == *repo_id);
        let sub_abs = active.map_or(&entry.path, |aw| &aw.path).join(sub_path);
        let submodules = active
            .map(|aw| aw.submodules.as_slice())
            .or_else(|| entry.status.as_ref().map(|s| s.submodules.as_slice()));
        let uninitialized = submodules
            .and_then(|modules| modules.iter().find(|sm| sm.path == sub_path))
            .and_then(|sm| sm.state.clone())
            == Some(crate::git::status::SubmoduleState::Uninitialized);
        if uninitialized {
            self.action_tx.send(Action::Error(format!(
                "Submodule {} is not initialized",
                sub_path.display()
            )))?;
            return Ok(());
        }
        let display_name = format!("{}/{}", entry.name, sub_path.display());
        self.activate_path_context(sub_abs, repo_id.clone(), display_name);
        self.github_touch_selection();
        Ok(())
    }

    /// Copy `text` to the system clipboard, reusing a long-lived `Clipboard`
    /// so the platform backend keeps serving the selection (see the field
    /// docs on `App::clipboard`). A failed or stale backend is dropped so the
    /// next copy attempt retries with a fresh connection.
    fn copy_to_clipboard(&mut self, text: &str) {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => {
                    self.error_message =
                        Some((format!("clipboard failed: {error}"), Instant::now()));
                    return;
                }
            }
        }
        let clipboard = self.clipboard.as_mut().expect("just initialized above");
        match clipboard.set_text(text) {
            Ok(()) => {
                self.success_message = Some(("Copied to clipboard".to_string(), Instant::now()));
            }
            Err(error) => {
                // The connection may be stale (e.g. compositor restarted);
                // drop it so the next copy retries from scratch.
                self.clipboard = None;
                self.error_message = Some((format!("clipboard failed: {error}"), Instant::now()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Component;
    use crate::git::status::{SubmoduleInfo, SubmoduleState, SubmoduleWarn};

    #[tokio::test]
    async fn selecting_a_worktree_submodule_uses_its_checkout_and_status() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let repo = git2::Repository::init(&parent).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        let linked = temp.path().join("linked");
        repo.worktree("linked", &linked, None).unwrap();
        let linked = linked.canonicalize().unwrap();
        git2::Repository::init(linked.join("child")).unwrap();
        let info = |state| SubmoduleInfo {
            name: "child".into(),
            path: "child".into(),
            state: Some(state),
            head: None,
            head_oid: None,
            workdir_oid: None,
            warn: SubmoduleWarn::default(),
        };
        let config = Config {
            root_dirs: vec![parent.clone()],
            write_target_override: Some(temp.path().join("config.toml")),
            ..Config::default()
        };
        let mut app = App::new(config).unwrap();
        let id = RepoId(parent.canonicalize().unwrap());
        let index = app.repo_list.resolve_index(&id).unwrap();
        let mut status = crate::git::status::query_status(&parent, &app.config.submodules).unwrap();
        status.submodules = vec![info(SubmoduleState::Uninitialized)];
        status.checkout_submodules = status.submodules.clone();
        status.worktree_info[0].checkout_submodules = vec![info(SubmoduleState::Dirty)];
        app.repo_list.update_status(index, status);
        app.repo_list.select_repo_row(index);
        for _ in 0..3 {
            app.repo_list
                .handle_key_event(crossterm::event::KeyCode::Down.into())
                .unwrap();
        }
        assert_eq!(
            app.selected_launch_path().unwrap().canonicalize().unwrap(),
            linked.join("child").canonicalize().unwrap()
        );
        app.handle_key_event(crossterm::event::KeyCode::Char('d').into())
            .unwrap();
        while let Ok(action) = app.action_rx.try_recv() {
            assert!(!matches!(
                action,
                Action::ConfirmRemoveRepo(_) | Action::RemoveWorktreeSelected
            ));
        }
        app.active_worktree = Some(ActiveWorktree {
            path: linked.clone(),
            repo_id: id.clone(),
            display_name: "linked".into(),
            submodules: vec![info(SubmoduleState::Dirty)],
        });

        app.select_submodule(&id, std::path::Path::new("child"))
            .unwrap();
        assert_eq!(
            app.active_worktree.as_ref().unwrap().path,
            linked.join("child")
        );

        app.active_worktree = Some(ActiveWorktree {
            path: linked.clone(),
            repo_id: id.clone(),
            display_name: "linked".into(),
            submodules: vec![info(SubmoduleState::Uninitialized)],
        });
        app.select_submodule(&id, std::path::Path::new("child"))
            .unwrap();
        assert_eq!(app.active_worktree.as_ref().unwrap().path, linked);
    }
}
