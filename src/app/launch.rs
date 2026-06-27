use super::*;

impl App {
    /// Run `git -C <repo_path> <args>` off-thread, marking the repo row busy and
    /// refreshing it on completion (via `GitOpComplete`). Used for worktree
    /// add/remove; mirrors the push/pull op flow.
    pub(super) fn spawn_repo_git_op(&mut self, repo_path: std::path::PathBuf, args: Vec<String>) {
        let repo_id = RepoId(repo_path.clone());
        if let Some(idx) = self.repo_list.resolve_index(&repo_id) {
            self.repo_list.repos[idx].git_op = true;
        }
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let guard = GitOpGuard::new(repo_id.clone(), tx.clone());
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path)
                .args(&args)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    guard.complete();
                    let _ = tx.send(Action::GitOpComplete {
                        id: repo_id,
                        message: format!("git {} succeeded", args.join(" ")),
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
                        args.join(" "),
                        first_line
                    )));
                    let _ = tx.send(Action::RefreshRepo(repo_id));
                }
                Err(e) => {
                    guard.complete();
                    let _ = tx.send(Action::Error(format!(
                        "git {} failed: {}",
                        args.join(" "),
                        crate::git::describe_spawn_error(&e)
                    )));
                    let _ = tx.send(Action::RefreshRepo(repo_id));
                }
            }
        });
    }
    /// Execute a [`crate::session::launcher::LaunchPlan`] for a verb (`open`/`review`)
    /// targeting `dir`. Needs `tui` so an `Inline` plan can suspend the TUI,
    /// run the command in the inherited terminal, and restore. Ask arrives in L3.
    pub(super) fn run_launch_plan(
        &mut self,
        plan: crate::session::launcher::LaunchPlan,
        dir: std::path::PathBuf,
        label: &'static str,
        tui: &mut Tui,
    ) -> Result<()> {
        use crate::session::launcher::LaunchPlan;
        match plan {
            LaunchPlan::Spawn(argv) => {
                spawn_detached(argv, dir, self.action_tx.clone(), label);
            }
            LaunchPlan::Inline(cmd) => {
                // Suspend the TUI, run the command in the inherited terminal so
                // an interactive viewer/editor works, then restore. `enter()` is
                // called unconditionally — a failed command must not leave us
                // suspended — and `clear()` forces a full repaint of the
                // re-entered alternate screen.
                tui.exit()?;
                let status = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .current_dir(&dir)
                    .status();
                tui.enter()?;
                tui.terminal.clear()?;
                // Set the error first, then render, so a failure's toast is
                // painted on the repaint rather than waiting for a later one.
                match status {
                    Err(e) => self.action_tx.send(Action::Error(format!(
                        "{label} failed: {}",
                        crate::git::describe_spawn_error(&e)
                    )))?,
                    Ok(st) if !st.success() => {
                        let code = st
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".to_string());
                        self.action_tx
                            .send(Action::Error(format!("{label} exited with status {code}")))?;
                    }
                    Ok(_) => {}
                }
                self.action_tx.send(Action::Render)?;
            }
            LaunchPlan::Ask => {
                self.action_tx
                    .send(Action::Error("ask placement isn't available yet".into()))?;
            }
            LaunchPlan::Error(msg) => {
                self.action_tx.send(Action::Error(msg))?;
            }
        }
        Ok(())
    }
    /// The directory of the highlighted row: a worktree row resolves to its own
    /// path, otherwise the selected repo's path (mirrors ShowGitGraph). Used by
    /// the keyboard `o`/`v` shortcuts; the context menu passes an explicit id.
    pub(super) fn selected_launch_path(&self) -> Option<std::path::PathBuf> {
        self.repo_list
            .selected_worktree()
            .map(|(_, wt)| wt.path.clone())
            .or_else(|| self.repo_list.selected_repo().map(|e| e.path.clone()))
    }
    /// Open `path` via the `[open]` launcher (or the placement picker for "ask").
    pub(super) fn launch_open(&mut self, path: std::path::PathBuf, tui: &mut Tui) -> Result<()> {
        let command = self.config.open.command.clone();
        let plan = crate::session::launcher::plan(
            command.as_deref(),
            &self.config.open.placement,
            &path.to_string_lossy(),
            None,
            std::env::var_os("TMUX").is_some(),
        );
        if matches!(plan, crate::session::launcher::LaunchPlan::Ask) {
            self.start_placement_picker(path, command, None, "open");
        } else {
            self.run_launch_plan(plan, path, "open", tui)?;
        }
        Ok(())
    }
    /// Review `path`'s diff vs its base ref via the `[review]` launcher. The base
    /// is explicit `[review] base`, else the repo's resolved default branch — no
    /// silent fallback, a doomed `git diff origin/HEAD...HEAD` errors clearly.
    pub(super) fn launch_review(&mut self, path: std::path::PathBuf, tui: &mut Tui) -> Result<()> {
        let base = self.config.review.base.clone().or_else(|| {
            git2::Repository::open(&path)
                .ok()
                .and_then(|r| crate::git::status::default_branch_name(&r))
        });
        let Some(base) = base else {
            self.action_tx.send(Action::Error(
                "no base branch resolved; set [review] base in config".into(),
            ))?;
            return Ok(());
        };
        let command = self
            .config
            .review
            .command
            .clone()
            .unwrap_or_else(|| "git diff {base}...HEAD".to_string());
        let plan = crate::session::launcher::plan(
            Some(&command),
            &self.config.review.placement,
            &path.to_string_lossy(),
            Some(&base),
            std::env::var_os("TMUX").is_some(),
        );
        if matches!(plan, crate::session::launcher::LaunchPlan::Ask) {
            self.start_placement_picker(path, Some(command), Some(base), "review");
        } else {
            self.run_launch_plan(plan, path, "review", tui)?;
        }
        Ok(())
    }
    /// Confirm removal of a worktree (the actual `git worktree remove` runs only
    /// after the user accepts the dialog).
    pub(super) fn confirm_remove_worktree(
        &mut self,
        repo: std::path::PathBuf,
        worktree_path: std::path::PathBuf,
        branch: String,
    ) -> Result<()> {
        self.confirm_dialog.show(
            format!("Remove worktree '{branch}'?"),
            Action::RemoveWorktree {
                repo,
                worktree_path,
            },
        );
        Ok(())
    }
    /// Park a launch and open the placement picker (`placement = "ask"`), listing
    /// the current tmux windows as right-of/below targets.
    pub(super) fn start_placement_picker(
        &mut self,
        dir: std::path::PathBuf,
        command: Option<String>,
        base: Option<String>,
        label: &'static str,
    ) {
        let choices =
            crate::session::launcher::placement_choices(&crate::session::launcher::tmux_windows());
        self.pending_pick = Some(PendingPick::Launch(PendingLaunch {
            dir,
            command,
            base,
            label,
        }));
        self.picker.show("Open where?", choices);
    }
    /// Attach the live tmux session(s) for the currently selected row (the `G`
    /// key path).
    pub(super) fn goto_session_selected(&mut self) -> Result<()> {
        let path = self
            .repo_list
            .selected_worktree()
            .map(|(_, wt)| wt.path.clone())
            .or_else(|| self.repo_list.selected_repo().map(|e| e.path.clone()));
        if let Some(path) = path {
            self.attach_sessions_for(&path)?;
        }
        Ok(())
    }
    /// Attach the live tmux session(s) at `path`: none -> a hint, one -> attach
    /// directly via the `[goto] command`, several -> the picker.
    pub(super) fn attach_sessions_for(&mut self, path: &std::path::Path) -> Result<()> {
        let sessions = crate::session::liveness::live_sessions(path, self.repo_list.live_panes());
        match sessions.as_slice() {
            [] => {
                self.action_tx
                    .send(Action::Error("no live tmux session here".into()))?;
            }
            [one] => self.goto_session(one),
            many => {
                let choices = many.iter().map(|s| (s.clone(), s.clone())).collect();
                let title =
                    match crate::session::launcher::goto_placement(&self.config.goto.command) {
                        Some(p) => format!("Open session ({p})"),
                        None => "Open session".to_string(),
                    };
                self.pending_pick = Some(PendingPick::GotoSession);
                self.picker.show(&title, choices);
            }
        }
        Ok(())
    }
    /// Run the `[goto] command` for `session`. The command returns promptly
    /// (switches the tmux client or spawns a terminal tab), so its exit status
    /// is checked and a failure (e.g. a stale session) is surfaced.
    pub(super) fn goto_session(&mut self, session: &str) {
        let argv = crate::session::launcher::build_goto_argv(&self.config.goto.command, session);
        if argv.is_empty() {
            // Unknown terminal (no detected new-tab/window command) and no
            // override: tell the user to configure it rather than switch in place.
            let _ = self.action_tx.send(Action::Error(
                "set [goto] command for your terminal (no new-tab/window command detected)".into(),
            ));
            return;
        }
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output();
            match output {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let first = stderr
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("command failed")
                        .trim();
                    let _ = tx.send(Action::Error(format!("goto failed: {first}")));
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!(
                        "goto failed: {}",
                        crate::git::describe_spawn_error(&e)
                    )));
                }
            }
        });
    }
    pub(super) fn spawn_refresh_query(&mut self, repo_id: RepoId) {
        let sub_cfg = self.config.submodules.clone();
        if let Some(idx) = self.repo_list.resolve_index(&repo_id) {
            self.pending_status.insert(repo_id.clone());
            let path = self.repo_list.repos[idx].path.clone();
            let tx = self.action_tx.clone();
            let sem = self.poll_semaphore.clone();
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
                            let _ = tx.send(Action::StatusQueryDone(repo_id.clone()));
                            // The watcher fires `RefreshRepo` on any change inside the repo,
                            // including `rm -rf <repo>` or `rm -rf <repo>/.git`. In that
                            // case the query naturally fails; surface a rescan instead of an
                            // error toast so the repo just disappears from the list.
                            if !path.join(".git").exists() {
                                tracing::debug!(
                                    "repo {} no longer a git repo; rescanning",
                                    path.display()
                                );
                                let _ = tx.send(Action::DiscoverNewRepos);
                            } else {
                                let _ = tx.send(Action::Error(format!("Failed to query: {}", e)));
                            }
                        }
                    }
                })
                .await
            });
        }
    }
    /// If a worktree is the active selection, refresh its graph and re-query
    /// its files so the changes panel updates live. Used by the local poll and
    /// after a git op completes on the worktree's parent repo (the parent
    /// status query updates the list row's ahead/behind, but the changes panel
    /// for a worktree is fed only by `WorktreeFilesLoaded`).
    pub(super) fn refresh_active_worktree(&mut self) {
        let Some(aw) = self.active_worktree.clone() else {
            return;
        };
        // Refresh the graph from the worktree so new commits appear live.
        if self.git_graph.has_detail() {
            self.git_graph.set_needs_reload();
        } else {
            self.git_graph.load_repo(aw.path.clone(), &aw.display_name);
        }

        let tx = self.action_tx.clone();
        let sub_cfg = self.config.submodules.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(s) = crate::git::status::query_status(&aw.path, &sub_cfg) {
                let _ = tx.send(Action::WorktreeFilesLoaded {
                    repo_id: aw.repo_id,
                    worktree_path: aw.path,
                    name: aw.display_name,
                    files: s.files,
                });
            }
        });
    }
}

/// Spawn `argv` detached in `cwd` with null stdio, reaping the child so a
/// fast-exiting launcher (tmux, GUI editor) does not linger as a zombie.
/// Spawn failures are surfaced via `Action::Error`, tagged with `label`
/// ("open" / "review"). Shared by every "launch a command" action.
fn spawn_detached(
    argv: Vec<String>,
    cwd: std::path::PathBuf,
    tx: UnboundedSender<Action>,
    label: &'static str,
) {
    let Some(program) = argv.first().cloned() else {
        let _ = tx.send(Action::Error(format!("{label} failed: empty command")));
        return;
    };
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        use std::process::Stdio;
        let child = std::process::Command::new(&program)
            .args(&argv[1..])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(mut c) => {
                // Drain stderr on a side thread so the pipe can't fill and block
                // the child before it exits (it exits promptly: tmux returns at
                // once, editor CLIs hand off and return).
                let stderr = c.stderr.take();
                let drain = std::thread::spawn(move || {
                    let mut buf = String::new();
                    if let Some(mut e) = stderr {
                        let _ = e.read_to_string(&mut buf);
                    }
                    buf
                });
                let status = c.wait();
                let err = drain.join().unwrap_or_default();
                // A failed tmux target ("can't find window") or quick-failing
                // launch would otherwise be silent.
                if let Ok(st) = status
                    && !st.success()
                {
                    let msg = err
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .map(str::trim)
                        .unwrap_or("command failed");
                    let _ = tx.send(Action::Error(format!("{label} failed: {msg}")));
                }
            }
            Err(e) => {
                let _ = tx.send(Action::Error(format!(
                    "{label} failed running '{program}': {}",
                    crate::git::describe_spawn_error(&e)
                )));
            }
        }
    });
}
