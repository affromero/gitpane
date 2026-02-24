use crossterm::event::{KeyEvent, MouseEvent};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Event {
    Init,
    Quit,
    Tick,
    Render,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    RepoChanged(usize),
    /// Periodic poll: refresh all repo statuses (local-only, no fetch)
    PollRefresh,
}
