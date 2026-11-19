use super::*;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub(super) struct Panel {
    id: RepoId,
    outcome: Option<&'static str>,
}

impl Panel {
    pub(super) fn draw(&self, frame: &mut ratatui::Frame, area: Rect, theme: &crate::theme::Theme) {
        let width = area.width.min(86);
        let height = area.height.min(18);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let overlay = &theme.overlay;
        let color = match self.outcome {
            Some("Fetched") => theme.status_bar.success_text,
            Some(_) => theme.status_bar.error_text,
            None => overlay.help_key,
        };
        let name: String = self
            .id
            .0
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_control())
            .take(280)
            .collect();
        let lines = vec![
            Line::raw(name),
            Line::raw(""),
            Line::styled(
                self.outcome.unwrap_or("Fetching…"),
                Style::default().fg(color),
            ),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Fetch diagnostics ")
            .title_bottom(" r Retry   Esc Close ")
            .border_style(Style::default().fg(overlay.help_border))
            .style(Style::default().bg(overlay.help_bg));
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
            rect,
        );
    }
}

impl App {
    pub(super) fn start_fetch_diagnostics(&mut self, id: RepoId) {
        let busy = self.pending_status.contains(&id)
            || self
                .repo_list
                .repos
                .iter()
                .any(|repo| repo.path == id.0 && repo.git_op);
        self.fetch_diagnostics = Some(Panel {
            id: id.clone(),
            outcome: busy.then_some("Repository is busy. Press r to retry."),
        });
        if busy {
            return;
        }
        self.pending_status.insert(id.clone());
        let tx = self.action_tx.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = fetch_one(&id.0);
            let _ = tx.send(Action::FetchDiagnosticsLoaded(id, outcome));
        });
    }

    pub(super) fn fetch_diagnostics_key(&mut self, key: KeyCode) -> Result<()> {
        match key {
            KeyCode::Esc => self.fetch_diagnostics = None,
            KeyCode::Char('r') => {
                if let Some(panel) = &self.fetch_diagnostics
                    && panel.outcome.is_some()
                {
                    self.start_fetch_diagnostics(panel.id.clone());
                }
            }
            _ => return Ok(()),
        }
        self.action_tx.send(Action::Render)?;
        Ok(())
    }

    pub(super) fn fetch_diagnostics_loaded(
        &mut self,
        id: RepoId,
        outcome: &'static str,
    ) -> Result<()> {
        self.pending_status.remove(&id);
        if let Some(panel) = &mut self.fetch_diagnostics
            && panel.id == id
        {
            panel.outcome = Some(outcome);
        }
        let changed_during_fetch = self.dirty_repos.remove(&id);
        if outcome == "Fetched" || changed_during_fetch {
            self.action_tx.send(Action::RefreshRepo(id))?;
        }
        Ok(())
    }
}

fn fetch_one(path: &std::path::Path) -> &'static str {
    let args = ["-c", "credential.interactive=never", "fetch", "--quiet"].map(str::to_owned);
    match crate::git::process::run_git_op_noninteractive(path, &args, Duration::from_secs(30)) {
        Ok(output) if output.status.success() => "Fetched",
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if stderr.contains("authentication failed")
                || stderr.contains("terminal prompts disabled")
                || stderr.contains("permission denied (publickey")
            {
                "Authentication failed. Check Git credentials and retry."
            } else if stderr.contains("could not resolve host")
                || stderr.contains("could not resolve hostname")
            {
                "Remote host could not be resolved. Check the remote and network."
            } else if stderr.contains("does not appear to be a git repository")
                || stderr.contains("repository not found")
            {
                "Remote repository unavailable. Check the remote path and access."
            } else {
                "Git fetch failed. Run git fetch in the repository for details."
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => "Git fetch timed out.",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "Git executable not found.",
        Err(_) => "Could not complete Git fetch.",
    }
}
