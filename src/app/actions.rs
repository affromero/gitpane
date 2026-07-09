use super::*;

impl App {
    pub(super) fn handle_action(&mut self, action: Action, tui: &mut Tui) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
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
            Action::FocusRepoDetails(ref id) => {
                // Same panel refresh as SelectRepo but without
                // moving the list selection — used by child rows
                // (currently stash entries) so the cursor can
                // remain on the child while the details panels
                // re-target onto that child's parent repo.
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
                }
                self.github_touch_selection();
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
            } => {
                if let Some(idx) = self.repo_list.resolve_index(repo_id) {
                    let entry = &self.repo_list.repos[idx];
                    let repo_name = entry.name.clone();
                    let sub_abs = entry.path.join(sub_path);
                    // Uninitialized submodules have no checked-out repo to graph.
                    let uninitialized = entry
                        .status
                        .as_ref()
                        .and_then(|s| s.submodules.iter().find(|sm| sm.path == *sub_path))
                        .and_then(|sm| sm.state.clone())
                        == Some(crate::git::status::SubmoduleState::Uninitialized);
                    if uninitialized {
                        self.action_tx.send(Action::Error(format!(
                            "Submodule {} is not initialized",
                            sub_path.display()
                        )))?;
                    } else {
                        let display_name = format!("{}/{}", repo_name, sub_path.display());
                        self.activate_path_context(sub_abs, repo_id.clone(), display_name);
                        self.github_touch_selection();
                    }
                }
            }
            Action::WorktreeFilesLoaded {
                ref repo_id,
                ref worktree_path,
                ref name,
                ref files,
            } => {
                // Only apply if this worktree is still selected
                if self
                    .active_worktree
                    .as_ref()
                    .is_some_and(|aw| aw.path == *worktree_path)
                {
                    self.file_list
                        .set_files(files.clone(), name, repo_id.clone());
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
                    let status_clone = status.clone();
                    self.repo_list.update_status(idx, status_clone);

                    // Refresh the file list so stale diffs are cleared
                    // when files are staged/unstaged. Skip when a worktree
                    // is being viewed — its files come from WorktreeFilesLoaded,
                    // not the parent repo's status.
                    if self.repo_list.selected_index() == Some(idx)
                        && self.active_worktree.is_none()
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
            Action::PollLocal => {
                // Probe tmux pane cwds once per poll so live
                // repos/worktrees get a marker (tmux-only; empty set
                // otherwise). One `tmux list-panes` call, off-thread.
                if self.config.ui.show_liveness && !self.liveness_probe_in_flight {
                    self.liveness_probe_in_flight = true;
                    let tx = self.action_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = tx.send(Action::LiveSessionsLoaded(
                            crate::session::liveness::tmux_pane_sessions(),
                        ));
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
                        tokio::task::spawn_blocking(move || match crate::git::status::query_status(
                            &path, &sub_cfg,
                        ) {
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
                            }
                        })
                        .await
                    });
                }

                // Also re-query the active worktree so its changes update live
                self.refresh_active_worktree();
            }
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
                        self.git_graph.load_repo(path, &name);
                    }
                }
                self.focus = FocusPanel::Graph;
            }
            Action::ShowFileList => {
                self.focus = FocusPanel::Changes;
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
                    let args = vec![
                        "worktree".to_string(),
                        "add".to_string(),
                        "-b".to_string(),
                        branch.clone(),
                        "--".to_string(),
                        new_path.to_string_lossy().to_string(),
                    ];
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
                let args = vec![
                    "worktree".to_string(),
                    "remove".to_string(),
                    "--".to_string(),
                    worktree_path.to_string_lossy().to_string(),
                ];
                self.spawn_repo_git_op(repo.clone(), args);
            }
            Action::LiveSessionsLoaded(panes) => {
                self.liveness_probe_in_flight = false;
                self.repo_list.set_live_panes(panes);
            }
            Action::PickerChose(ref value) => {
                self.picker.hide();
                match self.pending_pick.take() {
                    Some(PendingPick::Launch(p)) => {
                        let plan = crate::session::launcher::plan(
                            p.command.as_deref(),
                            value,
                            &p.dir.to_string_lossy(),
                            p.base.as_deref(),
                            std::env::var_os("TMUX").is_some(),
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
}
