use crossterm::event::{KeyEvent, MouseEvent};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Event {
    Init,
    Quit,
    Tick,
    Render,
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Bracketed-paste payload, routed to the focused text input.
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    RepoChanged(PathBuf),
    /// A direct child of one of the configured `root_dirs` was created,
    /// deleted, or moved. The app should re-run `scanner::discover_repos`
    /// (via `Action::DiscoverNewRepos`) after a short cooldown.
    ReposRootChanged,
    /// Fast local status poll (no network)
    PollLocal,
    /// Remote fetch poll (updates ahead/behind)
    PollFetch,
}
