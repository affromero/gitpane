//! Detects which repos/worktrees have a live tmux session by reading each
//! pane's session name and cwd. tmux-only: when gitpane is not under tmux (or
//! tmux is unavailable) the probe yields nothing and no markers are shown.

use std::path::{Path, PathBuf};

/// `(session_name, pane_cwd)` for every tmux pane across all sessions, via a
/// single `tmux list-panes -a` call. Empty when tmux is absent or errors.
pub(crate) fn tmux_pane_sessions() -> Vec<(String, PathBuf)> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_current_path}\t#{session_name}",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => parse_pane_sessions(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parse `<pane_cwd>\t<session>` lines into `(session, path)` pairs. The path is
/// first so a tab in a session name lands in (and stays in) the session field
/// rather than corrupting the path. Lines without a tab, an empty session, or
/// an empty path are skipped.
fn parse_pane_sessions(output: &str) -> Vec<(String, PathBuf)> {
    output
        .lines()
        .filter_map(|line| {
            let (path, session) = line.split_once('\t')?;
            let path = path.trim();
            let session = session.trim();
            if session.is_empty() || path.is_empty() {
                return None;
            }
            Some((session.to_string(), PathBuf::from(path)))
        })
        .collect()
}

/// Sorted, unique session names that have a pane cwd'd at or below `path` (the
/// repo/worktree is "live" in those sessions). Empty when none.
pub(crate) fn live_sessions(path: &Path, panes: &[(String, PathBuf)]) -> Vec<String> {
    let mut names: Vec<String> = panes
        .iter()
        .filter(|(_, p)| p.starts_with(path))
        .map(|(s, _)| s.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Marker label: the first live session name (the full list is on the context
/// menu). `None` when not live.
pub(crate) fn live_label(sessions: &[String]) -> Option<String> {
    sessions.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panes(ps: &[(&str, &str)]) -> Vec<(String, PathBuf)> {
        ps.iter()
            .map(|(s, p)| (s.to_string(), PathBuf::from(p)))
            .collect()
    }

    #[test]
    fn parse_skips_malformed_and_empty() {
        let out = "/code/app\tmain\nno-tab\n/code/x\t\n\twork\n/code/app/src\tside\n";
        assert_eq!(
            parse_pane_sessions(out),
            vec![
                ("main".to_string(), PathBuf::from("/code/app")),
                ("side".to_string(), PathBuf::from("/code/app/src")),
            ]
        );
    }

    #[test]
    fn parse_keeps_path_intact_when_session_name_has_tab() {
        // Path is the first field, so a tab in a session name can't corrupt it.
        assert_eq!(
            parse_pane_sessions("/code/app\tweird\tname\n"),
            vec![("weird\tname".to_string(), PathBuf::from("/code/app"))]
        );
    }

    #[test]
    fn live_sessions_are_prefix_matched_unique_and_sorted() {
        let p = panes(&[
            ("ternu", "/code/app/src"),
            ("fairtrail", "/code/app"),
            ("ternu", "/code/app"),         // duplicate session -> deduped
            ("other", "/code/app-sibling"), // sibling, not inside
        ]);
        assert_eq!(
            live_sessions(Path::new("/code/app"), &p),
            vec!["fairtrail".to_string(), "ternu".to_string()]
        );
    }

    #[test]
    fn live_sessions_empty_when_none_inside() {
        let p = panes(&[("x", "/elsewhere")]);
        assert!(live_sessions(Path::new("/code/app"), &p).is_empty());
    }

    #[test]
    fn live_label_is_first_session() {
        assert_eq!(live_label(&[]), None);
        assert_eq!(live_label(&["a".to_string()]), Some("a".to_string()));
        assert_eq!(
            live_label(&["a".to_string(), "b".to_string(), "c".to_string()]),
            Some("a".to_string())
        );
    }
}
