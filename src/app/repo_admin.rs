use super::*;

impl App {
    /// Repo-list administration: pinning and removing repos, sort order,
    /// and filesystem-driven rescans. Split from `handle_action_rest` so
    /// each dispatch file stays under the line cap.
    pub(super) fn handle_repo_admin(&mut self, action: Action) -> Result<()> {
        match action {
            Action::OpenAddRepo => {
                self.path_input.show();
            }
            Action::AddRepo(ref path) => {
                // Same predicate as pinned discovery: anything accepted here
                // must survive the next rescan (and vice versa).
                if !scanner::is_repo_root(path) {
                    // Keep the input open so the path can be corrected in place.
                    self.path_input.set_error("not a git repository");
                } else {
                    self.path_input.hide();
                    // Canonicalize so the entry matches the paths discovery
                    // emits — a symlinked or trailing-slash spelling would
                    // otherwise duplicate the repo on the next rescan.
                    let path = path.canonicalize().unwrap_or_else(|_| path.clone());
                    // Land the user on the result either way: Repos panel
                    // focused, new (or existing) row selected.
                    self.focus = FocusPanel::Repos;
                    if let Some(existing) = self.repo_list.repos.iter().find(|r| r.path == path) {
                        self.success_message =
                            Some(("already in list".to_string(), Instant::now()));
                        self.action_tx
                            .send(Action::SelectRepo(RepoId(existing.path.clone())))?;
                    } else {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.to_string_lossy().to_string());
                        self.config.add_pinned_repo(path.clone());
                        if let Err(e) = self.config.save() {
                            tracing::error!("Failed to save config: {}", e);
                            self.error_message =
                                Some((format!("save failed: {e}"), Instant::now()));
                        }
                        let repo_id = RepoId(path.clone());
                        let display = self.repo_list.display_for(&path);
                        self.repo_list.repos.push(RepoEntry {
                            path,
                            name,
                            display,
                            status: None,
                            git_op: false,
                        });
                        // Sort now: it rebuilds `display_rows`, so the
                        // SelectRepo below can land on the new row instead of
                        // silently no-op'ing against the stale row model.
                        self.sort_repos();
                        self.action_tx.send(Action::RefreshRepo(repo_id.clone()))?;
                        self.action_tx.send(Action::SelectRepo(repo_id))?;
                    }
                }
            }
            Action::ConfirmRemoveRepo(ref id) => {
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    let name = self.repo_list.repos[idx].name.clone();
                    self.confirm_dialog
                        .show(format!("Remove {name}?"), Action::RemoveRepo(id.clone()));
                }
            }
            Action::RemoveRepo(ref id) => {
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    // Clean up tracking sets for the removed repo
                    self.pending_status.remove(id);
                    self.dirty_repos.remove(id);
                    self.last_refresh.remove(id);
                    self.refresh_scheduled.remove(id);
                    let entry = &self.repo_list.repos[idx];
                    // Drop cached graph snapshots for the removed repo.
                    self.git_graph.invalidate_repo(&entry.path);
                    // Remove from pinned if it was pinned
                    self.config.pinned_repos.retain(|p| *p != entry.path);
                    // Exclude only repos the root walk can rediscover. A
                    // pinned submodule (inside another listed repo) or a repo
                    // outside every root never comes back on rescan, and its
                    // bare name in `excluded_repos` substring-matches
                    // unrelated paths.
                    let inside_listed_repo = self
                        .repo_list
                        .repos
                        .iter()
                        .any(|r| r.path != entry.path && entry.path.starts_with(&r.path));
                    // Compare against canonicalized roots: entry paths are
                    // canonical (macOS tempdirs resolve through /private).
                    let under_root = self.config.effective_root_dirs().iter().any(|root| {
                        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
                        entry.path.starts_with(&root)
                    });
                    let name = entry.name.clone();
                    if under_root
                        && !inside_listed_repo
                        && !self.config.excluded_repos.contains(&name)
                    {
                        self.config.excluded_repos.push(name);
                    }
                    // A removed repo can be the graph/changes panels' current
                    // path context (via "Open in graph"): drop it so the
                    // panels fall back to the selected repo.
                    if self
                        .active_worktree
                        .as_ref()
                        .is_some_and(|aw| aw.path.starts_with(&entry.path))
                    {
                        self.active_worktree = None;
                    }
                    if let Err(e) = self.config.save() {
                        tracing::error!("Failed to save config: {}", e);
                        self.error_message = Some((format!("save failed: {e}"), Instant::now()));
                    }
                    self.repo_list.repos.remove(idx);
                    // Fix selection
                    if self.repo_list.repos.is_empty() {
                        self.repo_list.state.select(None);
                        self.file_list
                            .set_files(Vec::new(), "", RepoId(std::path::PathBuf::new()));
                    } else {
                        let new_idx = idx.min(self.repo_list.repos.len() - 1);
                        self.repo_list.select_repo_row(new_idx);
                        let new_id = RepoId(self.repo_list.repos[new_idx].path.clone());
                        self.action_tx.send(Action::SelectRepo(new_id))?;
                    }
                }
            }
            Action::CycleSortOrder => {
                self.sort_order = self.sort_order.next();
                self.success_message =
                    Some((format!("sort: {}", self.sort_order.label()), Instant::now()));
                self.sort_repos();
                self.sync_selection();
            }
            Action::RescanRepos => {
                // Clear tracking sets — old paths are stale after rescan
                self.pending_status.clear();
                self.dirty_repos.clear();
                self.last_refresh.clear();
                self.refresh_scheduled.clear();
                // The repo set is rebuilt from scratch; cached graphs for
                // vanished paths are dead weight.
                self.git_graph.invalidate_graph_cache();
                // Clear user-added exclusions, save, and re-discover repos
                self.config.excluded_repos.clear();
                if let Err(e) = self.config.save() {
                    tracing::error!("Failed to save config: {}", e);
                    self.error_message = Some((format!("save failed: {e}"), Instant::now()));
                }
                // The list is rebuilt from scratch, so carry the selection
                // across by identity (a worktree subrow falls back to its
                // parent repo row — the fresh list has no statuses yet).
                let keep = self.repo_list.selected_row_id();
                let repo_paths = scanner::discover_repos(&self.config);
                self.repo_list = RepoList::new(
                    repo_paths,
                    self.config.effective_root_dirs().into_owned(),
                    self.config.ui.expand_worktrees,
                    self.theme.clone(),
                );
                self.repo_list
                    .register_action_handler(self.action_tx.clone())?;
                self.repo_list.init()?;
                self.rebuild_watcher();
                self.action_tx.send(Action::PollLocal)?;
                self.sort_repos();
                self.repo_list.resync_rows(keep);
                self.sync_selection();
            }
            Action::DiscoverNewRepos => {
                // Idempotent rescan: re-discover, but only mutate state
                // when the set actually changed. Preserves selection,
                // exclusions, in-flight queries, and dirty markers.
                // Clear the pending-trailing-edge flag and refresh the
                // cooldown anchor so back-to-back fires coalesce.
                self.discovery_pending = false;
                self.last_discovery = Some(Instant::now());
                let new_paths = scanner::discover_repos(&self.config);
                let diff = self.repo_list.sync_paths(new_paths);
                if diff.is_empty() {
                    // No-op: discovered set matches the current list.
                } else {
                    tracing::debug!(
                        "DiscoverNewRepos: +{} -{}",
                        diff.added.len(),
                        diff.removed.len()
                    );
                    // Re-register so newly-added rows hand mouse events back.
                    self.repo_list
                        .register_action_handler(self.action_tx.clone())?;
                    // Prune per-repo state for vanished repos so HashSets
                    // don't keep growing across the session.
                    self.prune_github_cache(&diff.removed);
                    for path in &diff.removed {
                        let id = RepoId(path.clone());
                        self.pending_status.remove(&id);
                        self.dirty_repos.remove(&id);
                        self.last_refresh.remove(&id);
                        self.refresh_scheduled.remove(&id);
                        // Drop cached graph snapshots for vanished repos.
                        self.git_graph.invalidate_repo(path);
                        if self
                            .active_worktree
                            .as_ref()
                            .is_some_and(|aw| aw.repo_id == id)
                        {
                            self.active_worktree = None;
                        }
                    }
                    // `sync_paths` and `sort_repos` each re-anchor the
                    // selection by row identity, so no manual restore here.
                    self.sort_repos();
                    self.rebuild_watcher();
                    // Queue status for newly added repos.
                    for path in &diff.added {
                        self.action_tx
                            .send(Action::RefreshRepo(RepoId(path.clone())))?;
                    }
                    self.sync_selection();
                }
            }
            _ => {}
        }
        Ok(())
    }
}
