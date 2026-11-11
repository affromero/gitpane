use std::path::Path;

use color_eyre::Result;

use crate::components::file_list::StashFileEntry;
use crate::git::stash::{self, StashSource};

/// Read the file list for one stash snapshot. This is the only TUI call site
/// of the shared stash reader; everything above it works with the view model.
pub(super) fn load_stash_files(repo: &Path, oid: &str) -> Result<Vec<StashFileEntry>> {
    let snapshot = stash::list_stash_files(repo, oid)?;
    Ok(snapshot
        .files
        .into_iter()
        .map(StashFileEntry::from_stash_file)
        .collect())
}

/// Read one file diff from the same immutable snapshot.
pub(super) fn load_stash_diff(
    repo: &Path,
    oid: &str,
    source: StashSource,
    path: &str,
) -> Result<String> {
    let diff = stash::stash_file_diff(repo, oid, source, path)?;
    if diff.trim().is_empty() {
        Ok("(no diff available)".to_string())
    } else {
        Ok(diff)
    }
}

impl super::App {
    /// Restore the file panel to the selected repo's working tree.
    fn restore_working_tree_files(&mut self) {
        let Some(idx) = self.repo_list.selected_index() else {
            self.file_list.set_files(
                Vec::new(),
                "",
                crate::repo_id::RepoId(std::path::PathBuf::new()),
            );
            return;
        };
        let Some(entry) = self.repo_list.repos.get(idx) else {
            return;
        };
        let name = entry.name.clone();
        let repo_id = crate::repo_id::RepoId(entry.path.clone());
        let files = entry
            .status
            .as_ref()
            .map(|status| status.files.clone())
            .unwrap_or_default();
        self.file_list.set_files(files, &name, repo_id);
        self.repo_list.select_repo_row(idx);
    }

    /// Leave stash mode and restore the selected repo's working-tree rows.
    pub(super) fn exit_stash_view(&mut self) -> Result<()> {
        self.restore_working_tree_files();
        Ok(())
    }

    /// Keep the stash preview consistent with the latest repo status:
    /// content stays bound to the OID, a renumber refreshes the display label,
    /// and a deleted stash drops back to the working tree instead of leaving a
    /// stale preview under the parent row.
    pub(super) fn reconcile_stash_preview(&mut self) {
        let Some(stash) = self.file_list.stash_view().cloned() else {
            return;
        };
        let Some(idx) = self.repo_list.resolve_index(&stash.repo_id) else {
            self.restore_working_tree_files();
            return;
        };
        let Some(status) = self
            .repo_list
            .repos
            .get(idx)
            .and_then(|entry| entry.status.as_ref())
        else {
            return;
        };
        match status.stashes.iter().find(|entry| entry.oid == stash.oid) {
            Some(entry) => {
                let label = format!("stash@{{{}}}", entry.index);
                let summary = entry.message.clone();
                if label != stash.label || summary != stash.summary {
                    self.file_list.update_stash_identity(
                        &stash.repo_id,
                        &stash.oid,
                        label,
                        summary,
                    );
                }
            }
            None => self.restore_working_tree_files(),
        }
    }
}
