//! Commit box actions: open the box, commit, and draft a message.

use super::*;

impl App {
    pub(super) fn handle_commit(&mut self, action: Action) -> Result<()> {
        match action {
            Action::OpenCommitBox => self.open_commit_box()?,
            Action::Commit {
                id,
                message,
                stage_all,
            } => self.start_commit(id, message, stage_all),
            Action::CommitFinished { id, result } => match result {
                Ok(_) => self.commit_box.committed(&id),
                Err(error) => self.report_commit_failure(&id, "commit", &error)?,
            },
            Action::DraftCommitMessage(id) => self.start_draft(id),
            Action::CommitDraftReady { id, result } => match result {
                Ok(message) => self.commit_box.fill(&id, &message),
                Err(error) => self.report_commit_failure(&id, "draft", &error)?,
            },
            _ => {}
        }
        Ok(())
    }

    fn open_commit_box(&mut self) -> Result<()> {
        let Some((id, name)) = self.file_list.working_tree() else {
            self.action_tx
                .send(Action::Error("Select a repository to commit".to_string()))?;
            return Ok(());
        };
        let (id, name) = (id.clone(), name.to_string());
        let staged = self.file_list.staged_count();
        let can_draft = self.config.commit.draft_command.is_some();
        self.commit_box.show(id, &name, staged, can_draft);
        Ok(())
    }

    /// Ask before committing with nothing staged; otherwise run Git off the UI
    /// thread with the spinner and quit guard pull and push use.
    fn start_commit(&mut self, id: RepoId, message: String, stage_all: bool) {
        let nothing_staged = self.file_list.working_tree().map(|(shown, _)| shown) == Some(&id)
            && self.file_list.staged_count() == 0;
        if !stage_all && nothing_staged {
            self.confirm_dialog.show(
                "Nothing staged. Stage all and commit?".to_string(),
                Action::Commit {
                    id,
                    message,
                    stage_all: true,
                },
            );
            return;
        }
        let Some(dir) = self.file_op_dir(&id) else {
            self.commit_box
                .failed(&id, "This repository is no longer in the list");
            return;
        };
        self.commit_box.set_committing();
        if let Some(idx) = self.repo_list.resolve_index(&id) {
            self.repo_list.repos[idx].git_op = true;
        }
        let tx = self.action_tx.clone();
        // Created before scheduling so a quit in between still waits for it.
        let guard = GitOpGuard::new(id.clone(), tx.clone());
        tokio::task::spawn_blocking(move || {
            let result = crate::git::commit::commit(&dir, &message, stage_all);
            guard.complete();
            let _ = match &result {
                Ok(summary) => tx.send(Action::GitOpComplete {
                    id: id.clone(),
                    message: format!("Committed {summary}"),
                }),
                Err(_) => tx.send(Action::RefreshRepo(id.clone())),
            };
            let _ = tx.send(Action::CommitFinished { id, result });
        });
    }

    fn start_draft(&mut self, id: RepoId) {
        let (Some(command), Some(dir)) = (
            self.config.commit.draft_command.clone(),
            self.file_op_dir(&id),
        ) else {
            self.commit_box
                .failed(&id, "Set [commit] draft_command to draft messages");
            return;
        };
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::git::commit::draft(&dir, &command);
            let _ = tx.send(Action::CommitDraftReady { id, result });
        });
    }

    /// Show a failure in the box, or in the status bar once the box has moved
    /// on to another checkout or closed.
    fn report_commit_failure(&mut self, id: &RepoId, what: &str, error: &str) -> Result<()> {
        if !self.commit_box.failed(id, error) {
            let first = error
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or(error)
                .trim();
            self.action_tx
                .send(Action::Error(format!("{what} failed: {first}")))?;
        }
        Ok(())
    }
}
