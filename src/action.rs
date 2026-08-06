use crate::git::graph::{DiffStat, GraphFilters, GraphRow};
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
    /// Retarget the changes + graph panels onto a changed submodule's own repo.
    /// `sub_path` is the submodule's repo-relative path (the changed-files row).
    SelectSubmodule {
        repo_id: RepoId,
        sub_path: std::path::PathBuf,
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
    /// Open a specific repo/worktree by id (from the context menu), bound to the
    /// right-clicked row so an async row re-sort can't retarget it.
    OpenAt(RepoId),
    /// Review the highlighted repo/worktree's diff vs its base branch in a new
    /// tmux window, via the `[review] command` (default `git diff {base}...HEAD`).
    /// Resolves the selection in-app, so it carries no payload.
    ReviewSelected,
    /// Review a specific repo/worktree by id (from the context menu), bound to
    /// the right-clicked row.
    ReviewAt(RepoId),
    /// Run the user-defined keybinding at this index in `config.keybindings`
    /// against the selected repo/worktree. Sent by its bound key.
    RunKeybinding(usize),
    /// Open the branch-name input to create a new worktree for the given repo.
    OpenNewWorktree(RepoId),
    /// Create a worktree for `repo` on a new branch `branch`
    /// (`git worktree add <dir> -b <branch>`).
    CreateWorktree {
        repo: std::path::PathBuf,
        branch: String,
    },
    /// Resolve the highlighted worktree and open a confirmation to remove it.
    /// Sent by the `d` key on a worktree row.
    RemoveWorktreeSelected,
    /// Resolve a specific worktree by id (from the context menu) and confirm
    /// removal, bound to the right-clicked row rather than the selection.
    RemoveWorktreeAt(RepoId),
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
    GraphFilterBranchesLoaded {
        generation: u64,
        branches: Vec<String>,
    },
    DiffStatsLoaded {
        generation: u64,
        stats: Vec<(git2::Oid, DiffStat)>,
    },
    /// Open the graph filter overlay from a graph-row context click.
    OpenGraphFilters,
    OpenGraphContextMenu,
    OpenGraphCommitFiles,
    OpenGraphSearch,
    ToggleGraphCollapse,
    ExpandAllGraphBranches,
    CopyText(String),
    /// Replace graph filters and rebuild the graph from the selected refs.
    SetGraphFilters(GraphFilters),
    SetGraphFirstParent(bool),
    ResetGraphFilters,
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
    /// Right-click on a changed-file row: open the file context menu. `id` is the
    /// changed-files box's repo id (the parent repo, even for worktree files).
    ShowFileContextMenu {
        id: RepoId,
        path: std::path::PathBuf,
        row: u16,
        col: u16,
        staged: bool,
        unstaged: bool,
        is_untracked: bool,
        is_submodule: bool,
    },
    /// Open a file in the configured `[open]` command, or the OS default app.
    OpenFile(RepoId, std::path::PathBuf),
    /// Reveal a file in the OS file manager (Finder on macOS).
    RevealFile(RepoId, std::path::PathBuf),
    /// Copy a changed file's absolute path to the clipboard.
    CopyFilePath(RepoId, std::path::PathBuf),
    StageFile(RepoId, std::path::PathBuf),
    UnstageFile(RepoId, std::path::PathBuf),
    /// Prompt before discarding. The `bool` is `is_untracked` (delete vs restore).
    DiscardFile(RepoId, std::path::PathBuf, bool),
    /// Run the discard after the confirm dialog accepts.
    DiscardFileConfirmed(RepoId, std::path::PathBuf, bool),
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
    /// The graph's file highlight moved. The app waits out the debounce and
    /// answers with [`Action::CommitDiffSettled`], so walking a file list runs
    /// one diff for where the highlight lands, not one per row it passes.
    ScheduleCommitDiff {
        generation: u64,
    },
    /// The file highlight has been still for the debounce window; diff it unless
    /// `generation` has been superseded by a later move.
    CommitDiffSettled {
        generation: u64,
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
    /// The selection settled after debouncing; fetch GitHub data for it if due.
    /// The `u64` is the selection generation, so a stale fire is ignored.
    GithubSelectionSettled(u64),
    /// Result of a `gh` issue/PR fetch for `repo_id`. Applied only when
    /// `generation` still matches the repo's latest request (staleness latch).
    GitHubFetched {
        repo_id: RepoId,
        generation: u64,
        result: Result<crate::git::github::GithubData, String>,
    },
    /// Open a URL in the OS browser (an issue/PR web link).
    OpenUrl(String),
    /// Cycle the GitHub panel's state filter (open → all → closed) and refetch.
    CycleGithubStateFilter,
    /// Load the body + comments of a github item into the panel's detail pane.
    /// `generation` latches out results for a superseded selection.
    ShowGithubItem {
        url: String,
        is_pr: bool,
        generation: u64,
    },
    /// Result of a [`Action::ShowGithubItem`] fetch.
    GithubItemLoaded {
        generation: u64,
        result: Result<crate::git::github::ItemDetail, String>,
    },
    /// Apply a theme transiently (live preview), no persistence.
    PreviewTheme(String),
    /// Apply a theme and write `theme = "<name>"` to the active config file.
    CommitTheme(String),
    /// Re-apply the theme that was active when the picker opened, then close.
    CancelThemePreview,
}
