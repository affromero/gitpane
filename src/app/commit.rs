//! Commit box actions and stage-all confirmation.

use super::*;
use crate::git::commit::CommitOutcome;

impl App {
    pub(super) fn handle_commit(&mut self, action: Action) -> Result<()> {
        match action {
            Action::OpenCommitBox => self.open_commit_box()?,
            Action::Commit {
                id,
                message,
                stage_all,
            } => self.start_commit(id, message, stage_all),
            Action::CommitFinished {
                id,
                message,
                result,
            } => {
                self.commit_box.finish_operation();
                match result {
                    Ok(CommitOutcome::Committed(_)) => self.commit_box.committed(&id),
                    Ok(CommitOutcome::NothingStaged)
                        if self.commit_box.target_id() == Some(&id) =>
                    {
                        self.confirm_dialog.show(
                            "Nothing staged. Stage all and commit?".to_string(),
                            Action::Commit {
                                id,
                                message,
                                stage_all: true,
                            },
                        );
                    }
                    Ok(CommitOutcome::NothingStaged) => {}
                    Err(error) => self.report_commit_failure(&id, &error)?,
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn open_commit_box(&mut self) -> Result<()> {
        if self.commit_box.is_committing() {
            return Ok(());
        }
        let Some((id, name)) = self.file_list.working_tree() else {
            self.action_tx
                .send(Action::Error("Select a repository to commit".to_string()))?;
            return Ok(());
        };
        let (id, name) = (id.clone(), name.to_string());
        let staged = self.file_list.staged_count();
        self.commit_box.show(id, &name, staged);
        Ok(())
    }

    /// Ask before committing with nothing staged; otherwise run Git off the UI
    /// thread with the spinner and quit guard pull and push use.
    fn start_commit(&mut self, id: RepoId, message: String, stage_all: bool) {
        if self.commit_box.is_committing() || self.commit_box.target_id() != Some(&id) {
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
                Ok(CommitOutcome::Committed(summary)) => tx.send(Action::GitOpComplete {
                    id: id.clone(),
                    message: format!("Committed {summary}"),
                }),
                Ok(CommitOutcome::NothingStaged) | Err(_) => {
                    tx.send(Action::RefreshRepo(id.clone()))
                }
            };
            let _ = tx.send(Action::CommitFinished {
                id,
                message,
                result,
            });
        });
    }

    /// Show a failure in the box, or in the status bar once the box has moved
    /// on to another checkout or closed.
    fn report_commit_failure(&mut self, id: &RepoId, error: &str) -> Result<()> {
        if !self.commit_box.failed(id, error) {
            let first = error
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or(error)
                .trim();
            self.action_tx
                .send(Action::Error(format!("Commit failed: {first}")))?;
        }
        Ok(())
    }
}
