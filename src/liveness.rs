//! Detects which repos/worktrees have a live session by reading tmux pane cwds.
//! tmux-only: when gitpane is not running under tmux (or tmux is unavailable)
//! the probe yields an empty set and no liveness markers are shown.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The current-working-directory of every tmux pane across all sessions, via a
/// single `tmux list-panes -a` call. Empty when tmux is absent or errors.
pub(crate) fn tmux_pane_paths() -> HashSet<PathBuf> {
    let output = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_current_path}"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => HashSet::new(),
    }
}

/// A repo/worktree at `path` is "live" if any tmux pane's cwd is at or below it
/// (an agent or shell is working inside it).
pub(crate) fn is_live(path: &Path, pane_paths: &HashSet<PathBuf>) -> bool {
    pane_paths.iter().any(|p| p.starts_with(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panes(ps: &[&str]) -> HashSet<PathBuf> {
        ps.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn live_when_a_pane_sits_inside_the_path() {
        let p = panes(&["/code/app/src", "/elsewhere"]);
        assert!(is_live(Path::new("/code/app"), &p));
    }

    #[test]
    fn live_when_a_pane_is_exactly_the_path() {
        let p = panes(&["/code/app"]);
        assert!(is_live(Path::new("/code/app"), &p));
    }

    #[test]
    fn not_live_when_no_pane_is_inside() {
        let p = panes(&["/code/other", "/code/app-sibling"]);
        // `/code/app-sibling` must NOT count as inside `/code/app` (component
        // boundary), and a separate worktree dir is its own subtree.
        assert!(!is_live(Path::new("/code/app"), &p));
    }

    #[test]
    fn not_live_with_no_panes() {
        assert!(!is_live(Path::new("/code/app"), &HashSet::new()));
    }
}
