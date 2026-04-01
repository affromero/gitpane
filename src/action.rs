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
    Error(String),
    UpdateAvailable(String),
    /// Clears the pending_status flag for a repo (sent on error paths)
    StatusQueryDone(RepoId),
}
