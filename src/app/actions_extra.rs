use super::*;

/// How long the graph's file highlight must sit still before its diff is read.
/// Walking the file list is a per-row `Repository::open` plus a tree-to-tree
/// diff, so only the row the user lands on is worth paying for.
const COMMIT_DIFF_DEBOUNCE: Duration = Duration::from_millis(200);

impl App {
    pub(super) fn handle_action_rest(&mut self, action: Action) -> Result<()> {
        match action {
            Action::GotoSessionSelected => {
                self.goto_session_selected()?;
            }
            Action::GotoSession(ref session) => {
                self.goto_session(session);
            }
            Action::GotoSessionPicker(ref id) => {
                self.attach_sessions_for(&id.0)?;
            }
            Action::GraphLoaded { generation, rows } => {
                if generation == self.git_graph.current_generation() {
                    self.git_graph.set_rows(rows);
                }
            }
            Action::GraphFilterBranchesLoaded {
                generation,
                branches,
            } => {
                if generation == self.git_graph.current_generation() {
                    self.git_graph.set_filter_branches(branches);
                }
            }
            Action::DiffStatsLoaded { generation, stats } => {
                if generation == self.git_graph.current_generation() {
                    self.git_graph.set_diff_stats(stats);
                }
            }
            Action::GraphError {
                generation,
                ref message,
            } => {
                // Drop stale errors so a failed build from a previous
                // repo/generation can't clobber the current graph.
                if generation == self.git_graph.current_generation() {
                    self.git_graph.set_error(message.clone());
                }
            }
            Action::GraphLoadAborted { generation } => {
                if generation == self.git_graph.current_generation() {
                    self.git_graph.abort_load();
                }
            }
            Action::OpenGraphFilters => {
                self.graph_filter_picker.show(
                    self.git_graph.filters(),
                    self.git_graph.filter_branches(),
                    self.git_graph.filter_authors(),
                    self.git_graph.first_parent(),
                );
            }
            Action::OpenGraphContextMenu => {
                if let Some((full_hash, short_hash, message)) =
                    self.git_graph.selected_commit_menu_data()
                {
                    self.graph_filter_picker.visible = false;
                    self.graph_context_menu.show(
                        full_hash,
                        short_hash,
                        message,
                        self.git_graph.first_parent(),
                        self.git_graph.can_toggle_selected_branch(),
                    );
                }
            }
            Action::OpenGraphCommitFiles => {
                if let Some(action) = self.git_graph.open_selected_commit_files() {
                    self.action_tx.send(action)?;
                }
            }
            Action::OpenGraphSearch => {
                self.git_graph.open_search();
            }
            Action::ToggleGraphCollapse => self.git_graph.toggle_selected_branch(),
            Action::ExpandAllGraphBranches => self.git_graph.expand_all(),
            Action::SetGraphFilters(filters) => {
                self.git_graph.set_filters(filters);
            }
            Action::SetGraphFirstParent(enabled) => {
                self.git_graph.set_first_parent(enabled);
            }
            Action::ResetGraphFilters => {
                self.git_graph
                    .set_filters(crate::git::graph::GraphFilters::default());
                self.git_graph.set_first_parent(false);
            }
            Action::ShowContextMenu {
                ref id,
                row,
                col,
                is_worktree,
            } => {
                let live_sessions =
                    crate::session::liveness::live_sessions(&id.0, self.repo_list.live_panes());
                if let Some(target) = self.repo_list.resolve_target(id) {
                    self.context_menu.show(
                        id.clone(),
                        col,
                        row,
                        crate::components::context_menu::MenuContext {
                            ahead: target.ahead,
                            behind: target.behind,
                            has_submodules: target.has_submodules,
                            is_worktree,
                            live_sessions,
                            goto_command: self.config.goto.command.clone(),
                        },
                    );
                }
            }
            Action::HideContextMenu => {
                self.context_menu.hide();
            }
            Action::ShowFileContextMenu {
                ref id,
                ref path,
                row,
                col,
                staged,
                unstaged,
                is_untracked,
                is_submodule,
            } => {
                self.context_menu.show_file(
                    id.clone(),
                    col,
                    row,
                    crate::components::context_menu::FileMenuContext {
                        path: path.clone(),
                        staged,
                        unstaged,
                        is_untracked,
                        is_submodule,
                    },
                );
            }
            Action::RevealFile(ref id, ref path) => {
                self.reveal_file_external(id, path);
            }
            Action::DiscardFile(ref id, ref path, is_untracked) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let message = if is_untracked {
                    format!("Delete untracked '{name}'? This cannot be undone.")
                } else {
                    format!("Discard changes to '{name}'? This cannot be undone.")
                };
                self.confirm_dialog.show(
                    message,
                    Action::DiscardFileConfirmed(id.clone(), path.clone(), is_untracked),
                );
            }
            Action::StageFile(ref id, ref path)
            | Action::UnstageFile(ref id, ref path)
            | Action::DiscardFileConfirmed(ref id, ref path, _) => {
                if let Some(dir) = self.file_op_dir(id) {
                    let mut args: Vec<String> = match action {
                        Action::StageFile(..) => vec!["add".into(), "-A".into()],
                        Action::UnstageFile(..) => vec!["reset".into(), "-q".into()],
                        Action::DiscardFileConfirmed(_, _, true) => {
                            vec!["clean".into(), "-fdq".into()]
                        }
                        Action::DiscardFileConfirmed(_, _, false) => {
                            vec!["restore".into(), "--staged".into(), "--worktree".into()]
                        }
                        _ => unreachable!(),
                    };
                    args.push("--".into());
                    args.push(path.to_string_lossy().into_owned());
                    self.spawn_git_op_in(dir, id.clone(), args);
                }
            }
            Action::CopyFilePath(ref id, ref path) => {
                if let Some(abs) = self.file_op_dir(id).map(|d| d.join(path)) {
                    let path_str = abs.to_string_lossy().to_string();
                    use std::io::Write;
                    let encoded = base64_encode(path_str.as_bytes());
                    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                    let _ = std::io::stdout().flush();
                }
            }
            Action::CopyPath(ref id) => {
                if let Some(target) = self.repo_list.resolve_target(id) {
                    let path_str = target.exec_path.to_string_lossy().to_string();
                    use std::io::Write;
                    let encoded = base64_encode(path_str.as_bytes());
                    let _ = write!(std::io::stdout(), "\x1b]52;c;{}\x1b\\", encoded);
                    let _ = std::io::stdout().flush();
                }
            }
            Action::GitPush(ref id)
            | Action::GitPull(ref id)
            | Action::GitPullRebase(ref id)
            | Action::GitPullSubmodules(ref id) => {
                if let Some(target) = self.repo_list.resolve_target(id) {
                    let branch = target.branch.clone();
                    let mut git_args: Vec<String> = match action {
                        Action::GitPush(_) => vec!["push".into()],
                        Action::GitPull(_) => vec!["pull".into()],
                        Action::GitPullRebase(_) => {
                            vec!["pull".into(), "--rebase".into()]
                        }
                        Action::GitPullSubmodules(_) => {
                            vec!["pull".into(), "--recurse-submodules".into()]
                        }
                        _ => unreachable!(),
                    };
                    // Add origin <branch> so pull/push works even without upstream config
                    if !branch.is_empty() && branch != "(no branch)" {
                        git_args.push("origin".into());
                        git_args.push(branch);
                    }
                    // The op runs in `exec_path` (the worktree's own
                    // directory when a worktree is targeted) but the
                    // parent repo's row shows the spinner and is
                    // refreshed afterward.
                    let parent_id = RepoId(self.repo_list.repos[target.parent_index].path.clone());
                    self.repo_list.repos[target.parent_index].git_op = true;
                    let path = target.exec_path.clone();
                    let tx = self.action_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let guard = GitOpGuard::new(parent_id.clone(), tx.clone());
                        let output = std::process::Command::new("git")
                            .arg("-C")
                            .arg(&path)
                            .args(&git_args)
                            .output();
                        match output {
                            Ok(o) if o.status.success() => {
                                guard.complete();
                                let _ = tx.send(Action::GitOpComplete {
                                    id: parent_id,
                                    message: format!("git {} succeeded", git_args.join(" ")),
                                });
                            }
                            Ok(o) => {
                                guard.complete();
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                let first_line = stderr
                                    .lines()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("unknown error")
                                    .trim();
                                let _ = tx.send(Action::Error(format!(
                                    "git {} failed: {}",
                                    git_args.join(" "),
                                    first_line
                                )));
                                let _ = tx.send(Action::RefreshRepo(parent_id));
                            }
                            Err(e) => {
                                guard.complete();
                                let _ = tx.send(Action::Error(format!(
                                    "git {} failed: {}",
                                    git_args.join(" "),
                                    crate::git::describe_spawn_error(&e)
                                )));
                                let _ = tx.send(Action::RefreshRepo(parent_id));
                            }
                        }
                    });
                }
            }
            Action::GitSubmoduleUpdate(ref id)
            | Action::GitSubmoduleSync(ref id)
            | Action::GitSubmoduleUpdateLatest(ref id) => {
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    let entry = &mut self.repo_list.repos[idx];
                    let git_args: Vec<String> = match action {
                        Action::GitSubmoduleUpdate(_) => {
                            ["submodule", "update", "--init", "--recursive"]
                                .iter()
                                .map(|s| s.to_string())
                                .collect()
                        }
                        Action::GitSubmoduleSync(_) => ["submodule", "sync"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        Action::GitSubmoduleUpdateLatest(_) => {
                            ["submodule", "foreach", "git", "pull", "origin", "HEAD"]
                                .iter()
                                .map(|s| s.to_string())
                                .collect()
                        }
                        _ => unreachable!(),
                    };
                    entry.git_op = true;
                    let path = entry.path.clone();
                    let repo_id = id.clone();
                    let tx = self.action_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let guard = GitOpGuard::new(repo_id.clone(), tx.clone());
                        let output = std::process::Command::new("git")
                            .arg("-C")
                            .arg(&path)
                            .args(&git_args)
                            .output();
                        match output {
                            Ok(o) if o.status.success() => {
                                guard.complete();
                                let _ = tx.send(Action::GitOpComplete {
                                    id: repo_id,
                                    message: format!("git {} succeeded", git_args.join(" ")),
                                });
                            }
                            Ok(o) => {
                                guard.complete();
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                let first_line = stderr
                                    .lines()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("unknown error")
                                    .trim();
                                let _ = tx.send(Action::Error(format!(
                                    "git {} failed: {}",
                                    git_args.join(" "),
                                    first_line
                                )));
                                let _ = tx.send(Action::RefreshRepo(repo_id));
                            }
                            Err(e) => {
                                guard.complete();
                                let _ = tx.send(Action::Error(format!(
                                    "git {} failed: {}",
                                    git_args.join(" "),
                                    crate::git::describe_spawn_error(&e)
                                )));
                                let _ = tx.send(Action::RefreshRepo(repo_id));
                            }
                        }
                    });
                }
            }
            Action::GitOpComplete {
                ref id,
                ref message,
            } => {
                self.success_message = Some((message.clone(), Instant::now()));
                self.action_tx.send(Action::RefreshRepo(id.clone()))?;
                // The parent refresh updates the list row's ahead/behind.
                // A worktree's changes panel is fed only by
                // WorktreeFilesLoaded, so if a worktree of this repo is
                // the active selection, refresh it now instead of
                // waiting for the next local poll.
                if self
                    .active_worktree
                    .as_ref()
                    .is_some_and(|aw| aw.repo_id == *id)
                {
                    self.refresh_active_worktree();
                }
            }
            Action::ShowDiff(ref id, ref file_path) => {
                if let Some(idx) = self.repo_list.resolve_index(id) {
                    let entry = &self.repo_list.repos[idx];
                    let diff_gen = self.file_list.diff_generation();
                    let sub_info = entry
                        .status
                        .as_ref()
                        .and_then(|s| s.submodules.iter().find(|sm| sm.path == *file_path));

                    if let Some(sub) = sub_info {
                        let repo_path = entry.path.clone();
                        let sub_path = file_path.clone();
                        let old_oid = sub.head_oid.clone().unwrap_or_default();
                        let new_oid = sub.workdir_oid.clone().unwrap_or_default();
                        let sub_state = sub.state.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let submodule_abs = repo_path.join(&sub_path);
                            let short_old = if old_oid.len() >= 7 {
                                &old_oid[..7]
                            } else {
                                &old_oid
                            };
                            let short_new = if new_oid.len() >= 7 {
                                &new_oid[..7]
                            } else {
                                &new_oid
                            };

                            // Dirty submodule (local uncommitted changes): show git diff
                            // Modified submodule (pointer changed): show commit log
                            let pointer_changed =
                                !old_oid.is_empty() && !new_oid.is_empty() && old_oid != new_oid;
                            let use_diff = sub_state
                                == Some(crate::git::status::SubmoduleState::Dirty)
                                || !pointer_changed;

                            if use_diff {
                                let label = match sub_state {
                                    Some(crate::git::status::SubmoduleState::Dirty) => {
                                        "uncommitted changes"
                                    }
                                    Some(crate::git::status::SubmoduleState::Uninitialized) => {
                                        "not initialized"
                                    }
                                    Some(crate::git::status::SubmoduleState::Modified) => {
                                        "modified"
                                    }
                                    None => "unpushed",
                                };
                                let header = format!(
                                    "Submodule {} ({})\n{}\n",
                                    sub_path.display(),
                                    label,
                                    "─".repeat(40),
                                );
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&submodule_abs)
                                    .args(["diff", "HEAD"])
                                    .output();
                                let body = match output {
                                    Ok(o) => {
                                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                                        if text.is_empty() {
                                            // Fallback: show status
                                            let status_out = std::process::Command::new("git")
                                                .arg("-C")
                                                .arg(&submodule_abs)
                                                .args(["status", "--short"])
                                                .output()
                                                .map(|o| {
                                                    String::from_utf8_lossy(&o.stdout).to_string()
                                                })
                                                .unwrap_or_default();
                                            if status_out.is_empty() {
                                                "(no changes detected)".to_string()
                                            } else {
                                                status_out
                                            }
                                        } else {
                                            text
                                        }
                                    }
                                    Err(e) => {
                                        format!(
                                            "Failed to get submodule diff: {}",
                                            crate::git::describe_spawn_error(&e)
                                        )
                                    }
                                };
                                let _ = tx.send(Action::DiffLoaded {
                                    generation: diff_gen,
                                    content: format!("{}{}", header, body),
                                });
                            } else {
                                // Pointer changed: show commit log between old and new
                                let header = format!(
                                    "Submodule {} → {}\n{}\n",
                                    short_old,
                                    short_new,
                                    "─".repeat(40),
                                );
                                let range = format!("{}..{}", old_oid, new_oid);
                                let output = std::process::Command::new("git")
                                    .arg("-C")
                                    .arg(&submodule_abs)
                                    .args(["log", "--oneline", "--graph", &range])
                                    .output();
                                let body = match output {
                                    Ok(o) => {
                                        let text = String::from_utf8_lossy(&o.stdout).to_string();
                                        if text.is_empty() {
                                            "(no commits in range)".to_string()
                                        } else {
                                            text
                                        }
                                    }
                                    Err(e) => format!(
                                        "Failed to get submodule log: {}",
                                        crate::git::describe_spawn_error(&e)
                                    ),
                                };
                                let _ = tx.send(Action::DiffLoaded {
                                    generation: diff_gen,
                                    content: format!("{}{}", header, body),
                                });
                            }
                        });
                    } else {
                        // Use worktree path for diffs when a worktree is selected
                        let path = self
                            .active_worktree
                            .as_ref()
                            .map(|aw| aw.path.clone())
                            .unwrap_or_else(|| entry.path.clone());
                        let fp = file_path.clone();
                        let tx = self.action_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let output = std::process::Command::new("git")
                                .arg("-C")
                                .arg(&path)
                                .arg("diff")
                                .arg("HEAD")
                                .arg("--")
                                .arg(&fp)
                                .output();
                            match output {
                                Ok(o) => {
                                    let mut text = String::from_utf8_lossy(&o.stdout).to_string();
                                    if text.is_empty() {
                                        text = String::from_utf8_lossy(&{
                                            std::process::Command::new("git")
                                                .arg("-C")
                                                .arg(&path)
                                                .arg("diff")
                                                .arg("--no-index")
                                                .arg("/dev/null")
                                                .arg(&fp)
                                                .output()
                                                .map(|o| o.stdout)
                                                .unwrap_or_default()
                                        })
                                        .to_string();
                                    }
                                    if text.is_empty() {
                                        text = "(no diff available)".to_string();
                                    }
                                    let _ = tx.send(Action::DiffLoaded {
                                        generation: diff_gen,
                                        content: text,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(Action::DiffLoaded {
                                        generation: diff_gen,
                                        content: format!(
                                            "Failed to get diff: {}",
                                            crate::git::describe_spawn_error(&e)
                                        ),
                                    });
                                }
                            }
                        });
                    }
                }
            }
            Action::DiffLoaded {
                generation,
                ref content,
            } => {
                if generation == self.file_list.diff_generation() {
                    self.file_list.set_diff(content.clone());
                }
            }
            Action::ShowCommitFiles {
                ref repo_path,
                ref oid,
            } => {
                let detail_gen = self.git_graph.current_detail_generation();
                let path = repo_path.clone();
                let oid = oid.clone();
                let tx = self.action_tx.clone();
                tokio::task::spawn_blocking(move || {
                    match crate::git::commit_files::list_commit_files(&path, &oid) {
                        Ok((message, files)) => {
                            let _ = tx.send(Action::CommitFilesLoaded {
                                generation: detail_gen,
                                oid,
                                message,
                                files,
                            });
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Action::Error(format!("Failed to list commit files: {}", e)));
                        }
                    }
                });
            }
            Action::CommitFilesLoaded {
                generation,
                ref oid,
                ref message,
                ref files,
            } => {
                if generation == self.git_graph.current_detail_generation()
                    && let Some(action) =
                        self.git_graph
                            .set_commit_files(oid.clone(), message.clone(), files.clone())
                {
                    self.action_tx.send(action)?;
                }
            }
            Action::ScheduleCommitDiff { generation } => {
                let tx = self.action_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(COMMIT_DIFF_DEBOUNCE).await;
                    let _ = tx.send(Action::CommitDiffSettled { generation });
                });
            }
            Action::CommitDiffSettled { generation } => {
                if let Some(action) = self.git_graph.commit_diff_settled(generation) {
                    self.action_tx.send(action)?;
                }
            }
            Action::ShowCommitDiff {
                ref repo_path,
                ref oid,
                ref file_path,
            } => {
                let detail_gen = self.git_graph.current_detail_generation();
                let path = repo_path.clone();
                let oid = oid.clone();
                let fp = file_path.clone();
                let tx = self.action_tx.clone();
                tokio::task::spawn_blocking(
                    move || match crate::git::commit_files::commit_file_diff(&path, &oid, &fp) {
                        Ok(diff) => {
                            let _ = tx.send(Action::CommitDiffLoaded {
                                generation: detail_gen,
                                content: diff,
                            });
                        }
                        Err(e) => {
                            let _ =
                                tx.send(Action::Error(format!("Failed to get commit diff: {}", e)));
                        }
                    },
                );
            }
            Action::CommitDiffLoaded {
                generation,
                ref content,
            } => {
                if generation == self.git_graph.current_detail_generation() {
                    self.git_graph.set_commit_diff(content.clone());
                }
            }
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
                        self.action_tx.send(Action::RefreshRepo(repo_id.clone()))?;
                        self.action_tx.send(Action::SelectRepo(repo_id))?;
                    }
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
                    // Remove from pinned if it was pinned
                    self.config.pinned_repos.retain(|p| *p != entry.path);
                    // Add to excluded so it won't reappear on rescan
                    let name = entry.name.clone();
                    if !self.config.excluded_repos.contains(&name) {
                        self.config.excluded_repos.push(name);
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
                    self.config.root_dirs.clone(),
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
            Action::UpdateAvailable(ref version) => {
                self.update_version = Some(version.clone());
            }
            Action::GithubSelectionSettled(generation) => {
                self.github_selection_settled(generation);
            }
            Action::GitHubFetched {
                repo_id,
                generation,
                result,
            } => {
                self.github_fetched(repo_id, generation, result);
            }
            Action::OpenUrl(ref url) => {
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
                self.os_open(vec![opener.to_string(), url.clone()], cwd, "github");
            }
            Action::ShowGithubItem {
                url,
                is_pr,
                generation,
            } => {
                let tx = self.action_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::git::github::fetch_detail(&url, is_pr);
                    let _ = tx.send(Action::GithubItemLoaded { generation, result });
                });
            }
            Action::GithubItemLoaded { generation, result } => {
                self.github_panel.set_detail(generation, result);
            }
            Action::CycleGithubStateFilter => {
                self.cycle_github_state_filter();
            }
            Action::Error(ref msg) => {
                tracing::error!("{}", msg);
                // Sanitize: single line, max 120 chars for status bar
                let clean: String = msg
                    .chars()
                    .map(|c| if c == '\n' { ' ' } else { c })
                    .collect();
                let truncated = if clean.len() > 120 {
                    format!("{}...", &clean[..117])
                } else {
                    clean
                };
                self.error_message = Some((truncated, Instant::now()));
            }
            Action::OpenThemePicker => {
                let env = crate::config::RealEnv;
                let dirs = self.config.theme_dirs(&env);
                let themes = discover_all_theme_names(&dirs);
                let current_name = self.config.effective_theme_name().to_string();
                let current_theme = self.theme.clone();
                self.theme_picker.show(themes, &current_name, current_theme);
            }
            Action::PreviewTheme(name) => {
                let env = crate::config::RealEnv;
                let dirs = self.config.theme_dirs(&env);
                match load_theme(&name, &dirs) {
                    Ok(t) => {
                        self.apply_theme(Arc::new(t));
                        // Preview routes through the session
                        // override so unrelated saves do not pin
                        // the previewed name to disk.
                        self.config.runtime_theme_override = Some(name);
                    }
                    Err(e) => {
                        tracing::warn!("theme preview failed: {e}");
                    }
                }
            }
            Action::CommitTheme(name) => {
                let env = crate::config::RealEnv;
                let dirs = self.config.theme_dirs(&env);
                match load_theme(&name, &dirs) {
                    Ok(t) => {
                        self.apply_theme(Arc::new(t));
                        // Commit is explicit: drop the runtime
                        // override and promote the choice to the
                        // persisted field.
                        self.config.runtime_theme_override = None;
                        self.config.theme_name = name.clone();
                        if let Err(e) = self.config.save() {
                            tracing::warn!("failed to persist theme: {e}");
                            self.error_message =
                                Some((format!("save failed: {e}"), Instant::now()));
                        } else {
                            self.success_message = Some((format!("theme: {name}"), Instant::now()));
                        }
                    }
                    Err(e) => {
                        self.error_message =
                            Some((format!("theme commit failed: {e}"), Instant::now()));
                    }
                }
                self.theme_picker.hide();
            }
            Action::CancelThemePreview => {
                // Restore the captured Arc<Theme> snapshot byte-for-
                // byte so cancel works even if the original theme
                // name no longer loads.
                if let Some(theme_snapshot) = self.theme_picker.original_theme() {
                    self.apply_theme(theme_snapshot);
                }
                // Re-establish the original override state. If the
                // captured "current" matches the persisted name,
                // there was no runtime override before the picker
                // opened; otherwise a `--theme` override was active.
                let original = self.theme_picker.original_name();
                match original {
                    Some(name) if name == self.config.theme_name => {
                        self.config.runtime_theme_override = None;
                    }
                    Some(name) => {
                        self.config.runtime_theme_override = Some(name);
                    }
                    None => {
                        self.config.runtime_theme_override = None;
                    }
                }
                self.theme_picker.hide();
            }
            _ => {
                let _ = self.repo_list.update(action)?;
            }
        }
        Ok(())
    }
}
