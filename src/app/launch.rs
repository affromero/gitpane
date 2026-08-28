use super::*;

impl App {
    /// Run `git -C <repo_path> <args>` off-thread, marking the repo row busy and
    /// refreshing it on completion (via `GitOpComplete`). Used for worktree
    /// add/remove; mirrors the push/pull op flow.
    pub(super) fn spawn_repo_git_op(&mut self, repo_path: std::path::PathBuf, args: Vec<String>) {
        let refresh_id = RepoId(repo_path.clone());
        self.spawn_git_op_in(repo_path, refresh_id, args);
    }

    /// Run `git -C <exec_path> <args>` off-thread, then refresh `refresh_id` via
    /// `GitOpComplete`. Splitting the working dir from the refresh target lets a
    /// worktree's file ops run in the worktree while the parent repo row (which
    /// owns the spinner and the changes panel) is the thing refreshed.
    pub(super) fn spawn_git_op_in(
        &mut self,
        exec_path: std::path::PathBuf,
        refresh_id: RepoId,
        args: Vec<String>,
    ) {
        if let Some(idx) = self.repo_list.resolve_index(&refresh_id) {
            self.repo_list.repos[idx].git_op = true;
        }
        let tx = self.action_tx.clone();
        // Created before scheduling so a quit between scheduling and closure
        // start still counts this op as in flight.
        let guard = GitOpGuard::new(refresh_id.clone(), tx.clone());
        let op_timeout = Duration::from_secs(self.config.git.op_timeout_secs);
        tokio::task::spawn_blocking(move || {
            let output = crate::git::status::run_git_op_capturing(&exec_path, &args, op_timeout);
            match output {
                Ok(o) if o.status.success() => {
                    guard.complete();
                    let _ = tx.send(Action::GitOpComplete {
                        id: refresh_id,
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
                    let _ = tx.send(Action::RefreshRepo(refresh_id));
                }
                Err(e) => {
                    guard.complete();
                    let _ = tx.send(Action::Error(format!(
                        "git {} failed: {}",
                        args.join(" "),
                        crate::git::describe_spawn_error(&e)
                    )));
                    let _ = tx.send(Action::RefreshRepo(refresh_id));
                }
            }
        });
    }

    /// Working directory for the changed-files box's current target: the active
    /// worktree's path when one is selected, else the repo's own path. Mirrors
    /// `ShowDiff` so file ops act on the same tree the diffs come from.
    pub(super) fn file_op_dir(&self, id: &RepoId) -> Option<std::path::PathBuf> {
        let idx = self.repo_list.resolve_index(id)?;
        Some(
            self.active_worktree
                .as_ref()
                .map(|aw| aw.path.clone())
                .unwrap_or_else(|| self.repo_list.repos[idx].path.clone()),
        )
    }

    /// Spawn an OS opener (`open` / `xdg-open`) detached. Shares the launch
    /// machinery used by `[open]`/`[review]`, just without a config template.
    pub(super) fn os_open(&self, argv: Vec<String>, cwd: std::path::PathBuf, label: &'static str) {
        spawn_detached(argv, cwd, self.action_tx.clone(), label);
    }

    /// Open a changed file: via the configured `[open]` command when set (so
    /// editor templates like `cursor {path}` work), else the OS default app.
    pub(super) fn open_file_external(
        &mut self,
        id: &RepoId,
        rel: &std::path::Path,
        tui: &mut Tui,
    ) -> Result<()> {
        let Some(abs) = self.file_op_dir(id).map(|d| d.join(rel)) else {
            return Ok(());
        };
        if self.config.open.command.is_some() {
            self.launch_open(abs, tui)?;
        } else {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let cwd = abs
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| abs.clone());
            self.os_open(
                vec![opener.to_string(), abs.to_string_lossy().into_owned()],
                cwd,
                "open",
            );
        }
        Ok(())
    }

    /// Reveal a changed file in the OS file manager: Finder selects it on macOS
    /// (`open -R`); elsewhere open the enclosing folder (`xdg-open <dir>`).
    pub(super) fn reveal_file_external(&self, id: &RepoId, rel: &std::path::Path) {
        let Some(abs) = self.file_op_dir(id).map(|d| d.join(rel)) else {
            return;
        };
        let parent = abs
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| abs.clone());
        let argv = if cfg!(target_os = "macos") {
            vec![
                "open".to_string(),
                "-R".to_string(),
                abs.to_string_lossy().into_owned(),
            ]
        } else {
            vec![
                "xdg-open".to_string(),
                parent.to_string_lossy().into_owned(),
            ]
        };
        self.os_open(argv, parent, "reveal");
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
            LaunchPlan::Herdr { create, command } => {
                self.spawn_herdr_launch(create, command, dir, label);
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

    /// herdr launch: run `create` (`herdr pane split` / `herdr tab create`),
    /// parse the new pane's id from its JSON response, then run `command` in
    /// that pane with `herdr pane run`. Runs off-thread; failures surface via
    /// `Action::Error`.
    pub(super) fn spawn_herdr_launch(
        &mut self,
        create: Vec<String>,
        command: Option<String>,
        dir: std::path::PathBuf,
        label: &'static str,
    ) {
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let Some(program) = create.first().cloned() else {
                let _ = tx.send(Action::Error(format!(
                    "{label} failed: empty herdr command"
                )));
                return;
            };
            let output = std::process::Command::new(&program)
                .args(&create[1..])
                .current_dir(&dir)
                .output();
            let Ok(out) = output else {
                let _ = tx.send(Action::Error(format!("{label} failed running herdr")));
                return;
            };
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let first = stderr
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("herdr failed")
                    .trim();
                let _ = tx.send(Action::Error(format!("{label} failed: {first}")));
                return;
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            if let Some(cmd) = command {
                // Parse only when the pane id is actually needed: the default
                // `o` path passes `command: None`, so an output-shape
                // mismatch there must not fail the launch (the pane or tab is
                // already open).
                let Some(pane_id) = crate::session::launcher::parse_herdr_pane_id(&stdout) else {
                    let _ = tx.send(Action::Error(format!(
                        "{label} failed: no pane id in herdr response (stdout: {})",
                        sanitize_stdout(&stdout)
                    )));
                    return;
                };
                let run = std::process::Command::new("herdr")
                    .args(["pane", "run", &pane_id, &cmd])
                    .current_dir(&dir)
                    .output();
                match run {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        let first = stderr
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("herdr failed")
                            .trim();
                        let _ = tx.send(Action::Error(format!("{label} failed: {first}")));
                    }
                    Err(e) => {
                        let _ = tx.send(Action::Error(format!(
                            "{label} failed: {}",
                            crate::git::describe_spawn_error(&e)
                        )));
                    }
                }
            }
        });
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
            self.mux,
        );
        if matches!(plan, crate::session::launcher::LaunchPlan::Ask) {
            self.start_placement_picker(path, command, None, "open");
        } else {
            self.run_launch_plan(plan, path, "open", tui)?;
        }
        Ok(())
    }
    /// Run the user keybinding at `idx` against `path` via the shared launcher.
    /// Mirrors [`Self::launch_open`]; only `{path}` is substituted (there is no
    /// review base), and `ask` placement routes through the placement picker.
    pub(super) fn launch_keybinding(
        &mut self,
        idx: usize,
        path: std::path::PathBuf,
        tui: &mut Tui,
    ) -> Result<()> {
        let Some(kb) = self.config.keybindings.get(idx) else {
            return Ok(());
        };
        let command = kb.command.clone();
        let placement = kb.placement.clone();
        let plan = crate::session::launcher::plan(
            Some(&command),
            &placement,
            &path.to_string_lossy(),
            None,
            self.mux,
        );
        if matches!(plan, crate::session::launcher::LaunchPlan::Ask) {
            self.start_placement_picker(path, Some(command), None, "keybinding");
        } else {
            self.run_launch_plan(plan, path, "keybinding", tui)?;
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
            self.mux,
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
    /// tmux windows as right-of/below targets, or herdr tabs/splits under herdr.
    pub(super) fn start_placement_picker(
        &mut self,
        dir: std::path::PathBuf,
        command: Option<String>,
        base: Option<String>,
        label: &'static str,
    ) {
        let choices = match self.mux {
            crate::session::env::Multiplexer::Herdr => {
                crate::session::launcher::herdr_placement_choices()
            }
            _ => crate::session::launcher::placement_choices(
                &crate::session::launcher::tmux_windows(),
            ),
        };
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
    /// Attach the live session(s) at `path`: none -> a hint, one -> attach
    /// directly (a herdr tab focus, or the tmux `[goto] command`), several -> the
    /// picker.
    pub(super) fn attach_sessions_for(&mut self, path: &std::path::Path) -> Result<()> {
        let mux = self.mux;
        let sessions = crate::session::liveness::live_sessions(path, self.repo_list.live_panes());
        match sessions.as_slice() {
            [] => {
                let msg = if mux == crate::session::env::Multiplexer::Herdr {
                    "no live herdr tab here"
                } else {
                    "no live tmux session here"
                };
                self.action_tx.send(Action::Error(msg.into()))?;
            }
            [one] => self.goto_session(one),
            many => {
                let choices = many.iter().map(|s| (s.clone(), s.clone())).collect();
                let title = if mux == crate::session::env::Multiplexer::Herdr {
                    "Open herdr tab".to_string()
                } else {
                    match crate::session::launcher::goto_placement(&self.config.goto.command) {
                        Some(p) => format!("Open session ({p})"),
                        None => "Open session".to_string(),
                    }
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
        // herdr: focus the tab hosting the live pane — no `[goto] command` needed.
        if self.mux == crate::session::env::Multiplexer::Herdr {
            let tx = self.action_tx.clone();
            let session = session.to_string();
            tokio::task::spawn_blocking(move || {
                let output = std::process::Command::new("herdr")
                    .args(["tab", "focus", &session])
                    .output();
                match output {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        let first = stderr
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("herdr failed")
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
            return;
        }
        // tmux / plain terminal: the `[goto] command` opens a terminal tab/window.
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
    /// Point the detail panels (changes + graph) at `path` as the active
    /// context, clearing the file list while a background status query loads
    /// its changes. Shared by worktree and submodule selection — both retarget
    /// the panels onto a git repo that is not the selected list row.
    pub(super) fn activate_path_context(
        &mut self,
        path: std::path::PathBuf,
        repo_id: RepoId,
        display_name: String,
    ) {
        self.context_menu.hide();
        self.active_worktree = Some(ActiveWorktree {
            path: path.clone(),
            repo_id: repo_id.clone(),
            display_name: display_name.clone(),
        });
        // Clear the file list while loading (parent repo_id for resolve_index).
        self.file_list
            .set_files(Vec::new(), &display_name, repo_id.clone());
        self.git_graph.load_repo(path.clone(), &display_name);

        let tx = self.action_tx.clone();
        let sub_cfg = self.config.submodules.clone();
        tokio::task::spawn_blocking(move || {
            match crate::git::status::query_status(&path, &sub_cfg) {
                Ok(s) => {
                    let _ = tx.send(Action::WorktreeFilesLoaded {
                        repo_id,
                        worktree_path: path,
                        name: display_name,
                        files: s.files,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Action::Error(format!("Status query: {}", e)));
                }
            }
        });
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
        // Bypass the cache: this runs on the local poll and after git ops on
        // the parent, so it must read current on-disk state. While a commit
        // detail is open the reload is deferred until it closes, so drop the
        // cached snapshot for the worktree path now — otherwise the deferred
        // `reload_graph` would resurrect the stale rows from cache.
        self.git_graph.invalidate_repo(&aw.path);
        if self.git_graph.has_detail() {
            self.git_graph.set_needs_reload();
        } else {
            self.git_graph
                .force_reload_repo(aw.path.clone(), &aw.display_name);
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

/// Truncate and escape a command's stdout for inclusion in an error message.
/// Keeps the message single-line, bounded, and free of control characters.
fn sanitize_stdout(stdout: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut out = String::new();
    for ch in stdout.chars().take(MAX_CHARS) {
        match ch {
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    if stdout.chars().count() > MAX_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize_stdout;

    #[test]
    fn sanitize_stdout_truncates_and_escapes_controls() {
        let long = "x".repeat(300);
        let s = sanitize_stdout(&long);
        assert_eq!(s.chars().count(), 201);
        assert!(s.ends_with('…'));

        let s = sanitize_stdout("a\nb\r\tc\u{1b}[31m");
        assert_eq!(s, "a b  c?[31m");
    }
}
