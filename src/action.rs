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
    Error(String),
}
