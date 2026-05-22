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
    /// Fast local status poll (no spinner, no fetch)
    PollLocal,
    /// Remote fetch poll (no spinner)
    PollFetch,
    ShowGitGraph,
    ShowFileList,
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
    GraphError(String),
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
