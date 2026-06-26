use crate::git::graph::{DiffStat, GraphRow};
use crate::git::status::RepoStatus;
use crate::repo_id::RepoId;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum Action {
    Tick,
    Render,
    Quit,
    Resize(u16, u16),
    SelectNextRepo,
    SelectPrevRepo,
    SelectRepo(RepoId),
    /// Update the file list and graph to a repo's data without moving the
    /// list-row selection. Used when a child row (stash entry) is highlighted.
    FocusRepoDetails(RepoId),
    SelectWorktree {
        repo_id: RepoId,
        worktree_path: std::path::PathBuf,
        worktree_branch: String,
    },
    /// Carries the result of a worktree status query back to the UI.
    WorktreeFilesLoaded {
        repo_id: RepoId,
        worktree_path: std::path::PathBuf,
        name: String,
        files: Vec<crate::git::status::FileEntry>,
    },
    RepoStatusUpdated {
        id: RepoId,
        status: RepoStatus,
    },
    RefreshAll,
    RefreshRepo(RepoId),
    /// Deferred watcher refresh fired after the per-repo cooldown expires.
    RefreshRepoAfterCooldown(RepoId),
    /// Fast local status poll (no spinner, no fetch)
    PollLocal,
    /// Remote fetch poll (no spinner)
    PollFetch,
    ShowGitGraph,
    ShowFileList,
    /// Open the highlighted repo (or worktree) in a new tmux pane, or the
    /// configured `[open] command`. Resolves the selection in-app, like
    /// `ShowGitGraph`, so it carries no payload.
    OpenSelected,
    /// Review the highlighted repo/worktree's diff vs its base branch in a new
    /// tmux window, via the `[review] command` (default `git diff {base}...HEAD`).
    /// Resolves the selection in-app, so it carries no payload.
    ReviewSelected,
    /// Open the branch-name input to create a new worktree for the given repo.
    OpenNewWorktree(RepoId),
    /// Create a worktree for `repo` on a new branch `branch`
    /// (`git worktree add <dir> -b <branch>`).
    CreateWorktree {
        repo: std::path::PathBuf,
        branch: String,
    },
    /// Resolve the highlighted worktree and open a confirmation to remove it.
    /// Sent by the `d` key on a worktree row and the context-menu item.
    RemoveWorktreeSelected,
    /// Remove `worktree_path` from `repo` (`git worktree remove <path>`).
    RemoveWorktree {
        repo: std::path::PathBuf,
        worktree_path: std::path::PathBuf,
    },
    /// `(session, pane_cwd)` from the latest liveness probe; repos/worktrees
    /// whose path contains a pane cwd are marked live with that session's name.
    LiveSessionsLoaded(Vec<(String, std::path::PathBuf)>),
    /// The picker chose this value (a placement string, or a session name);
    /// the app routes it by its pending picker purpose.
    PickerChose(String),
    /// The picker was dismissed; drop the pending action.
    PickerCancel,
    /// Attach the tmux session(s) live in the selected repo/worktree via the
    /// `[goto] command` (default `tmux switch-client -t {session}`). One session
    /// goes directly; several open the picker. Triggered by the `G` key.
    GotoSessionSelected,
    /// Attach a named tmux session directly (from a context-menu "Attach
    /// <session>" item) via the `[goto] command`.
    GotoSession(String),
    /// Open the picker for the tmux sessions live in this repo/worktree path
    /// (from the context-menu "Attach session…" item). Path-bound so it targets
    /// the clicked row, not the current selection.
    GotoSessionPicker(RepoId),
    GraphLoaded {
        generation: u64,
        rows: Vec<GraphRow>,
    },
    DiffStatsLoaded {
        generation: u64,
        stats: Vec<(git2::Oid, DiffStat)>,
    },
    ShowContextMenu {
        id: RepoId,
        row: u16,
        col: u16,
        /// True when the right-clicked row is a worktree (vs a top-level repo),
        /// so the menu can offer worktree-specific items.
        is_worktree: bool,
    },
    HideContextMenu,
    CopyPath(RepoId),
    GitPush(RepoId),
    GitPull(RepoId),
    GitPullRebase(RepoId),
    GitPullSubmodules(RepoId),
    GitSubmoduleUpdate(RepoId),
    GitSubmoduleSync(RepoId),
    GitSubmoduleUpdateLatest(RepoId),
    GitOpComplete {
        id: RepoId,
        message: String,
    },
    ShowDiff(RepoId, std::path::PathBuf),
    DiffLoaded {
        generation: u64,
        content: String,
    },
    GraphError {
        generation: u64,
        message: String,
    },
    /// A background graph build exited without sending `GraphLoaded`/`GraphError`
    /// (e.g. it panicked). Releases the `load_in_flight` latch for `generation`
    /// so the graph can rebuild on the next change instead of freezing.
    GraphLoadAborted {
        generation: u64,
    },
    ShowCommitFiles {
        repo_path: std::path::PathBuf,
        oid: String,
    },
    CommitFilesLoaded {
        generation: u64,
        oid: String,
        message: String,
        files: Vec<(String, String)>,
    },
    ShowCommitDiff {
        repo_path: std::path::PathBuf,
        oid: String,
        file_path: String,
    },
    CommitDiffLoaded {
        generation: u64,
        content: String,
    },
    OpenAddRepo,
    AddRepo(std::path::PathBuf),
    RemoveRepo(RepoId),
    CycleSortOrder,
    RescanRepos,
    /// Idempotent rescan triggered by FS events. Compares the discovered set
    /// against the current set and only mutates state on a diff. Unlike
    /// [`Action::RescanRepos`] this preserves `excluded_repos`, in-flight
    /// status queries, dirty-repo tracking, and the user's selection.
    DiscoverNewRepos,
    Error(String),
    UpdateAvailable(String),
    /// Clears the pending_status flag for a repo (sent on error paths)
    StatusQueryDone(RepoId),
    /// Open the in-app theme picker overlay.
    OpenThemePicker,
    /// Apply a theme transiently (live preview), no persistence.
    PreviewTheme(String),
    /// Apply a theme and write `theme = "<name>"` to the active config file.
    CommitTheme(String),
    /// Re-apply the theme that was active when the picker opened, then close.
    CancelThemePreview,
}
