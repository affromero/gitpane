use crate::git::graph::GraphRow;
use crate::git::status::RepoStatus;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum Action {
    Tick,
    Render,
    Quit,
    Resize(u16, u16),
    SelectNextRepo,
    SelectPrevRepo,
    SelectRepo(usize),
    RepoStatusUpdated { index: usize, status: RepoStatus },
    RefreshAll,
    RefreshRepo(usize),
    ShowGitGraph,
    ShowFileList,
    GraphLoaded(Vec<GraphRow>),
    ShowContextMenu { index: usize, row: u16, col: u16 },
    HideContextMenu,
    CopyPath(usize),
    Error(String),
}
