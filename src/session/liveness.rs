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
        Ok(o) if o.status.success() => parse_pane_sessions(&String::from_utf8_lossy(&o.stdout))
            .into_iter()
            // Canonicalize each pane cwd so it matches the canonicalized repo /
            // worktree paths even through symlinks (e.g. macOS /tmp ->
            // /private/tmp). Fall back to the raw path if it no longer exists.
            .map(|(s, p)| {
                let canon = p.canonicalize().unwrap_or(p);
                (s, canon)
            })
            .collect(),
        Ok(o) => {
            rate_limited_debug(|| {
                format!(
                    "tmux list-panes failed (status {:?}): {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            });
            Vec::new()
        }
        Err(e) => {
            rate_limited_debug(|| format!("tmux list-panes spawn failed: {e}"));
            Vec::new()
        }
    }
}

/// Log a debug line at most once per minute. The liveness probes run every
/// poll (5s by default), so a persistently failing probe would otherwise
/// spam the log; the rate limit keeps the failure diagnosable without noise.
fn rate_limited_debug(msg: impl FnOnce() -> String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 60
        && LAST
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::debug!("{}", msg());
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

/// Probe herdr panes: `(tab_id, pane_cwd)` for every pane that reports a cwd.
/// The tab id is the handle herdr's attach (`herdr tab focus`) takes, the herdr
/// analog of a tmux session name. Prefers `foreground_cwd` (what a running
/// agent is actually working in) over the pane's label cwd. Empty when herdr
/// is absent or errors.
pub(crate) fn herdr_live_panes() -> Vec<(String, PathBuf)> {
    let output = std::process::Command::new("herdr")
        .args(["pane", "list"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            match parse_herdr_panes(&String::from_utf8_lossy(&o.stdout)) {
                Ok(panes) => panes
                    .into_iter()
                    .map(|(s, p)| {
                        let canon = p.canonicalize().unwrap_or(p);
                        (s, canon)
                    })
                    .collect(),
                Err(e) => {
                    rate_limited_debug(|| format!("herdr pane list parse failed: {e}"));
                    Vec::new()
                }
            }
        }
        Ok(o) => {
            rate_limited_debug(|| {
                format!(
                    "herdr pane list failed (status {:?}): {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            });
            Vec::new()
        }
        Err(e) => {
            rate_limited_debug(|| format!("herdr pane list spawn failed: {e}"));
            Vec::new()
        }
    }
}

/// Parse `herdr pane list` JSON (`.result.panes[]` of `{tab_id, cwd,
/// foreground_cwd}`) into `(tab_id, cwd)` pairs, dropping panes with no
/// resolvable cwd or tab id. `Err` carries the serde error so a shape
/// mismatch is diagnosable instead of collapsing to an empty set.
fn parse_herdr_panes(output: &str) -> Result<Vec<(String, PathBuf)>, String> {
    #[derive(serde::Deserialize)]
    struct Pane {
        #[serde(default)]
        tab_id: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        foreground_cwd: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Panes {
        panes: Vec<Pane>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        result: Panes,
    }
    let parsed: Envelope = serde_json::from_str(output).map_err(|e| e.to_string())?;
    Ok(parsed
        .result
        .panes
        .into_iter()
        .filter_map(|p| {
            // Prefer the foreground cwd (what a running agent is actually
            // working in); fall back to the pane's label cwd. Treat an empty
            // string as absent so `Some("")` falls through to `cwd` instead
            // of producing a bogus root-relative path.
            let cwd = p
                .foreground_cwd
                .filter(|c| !c.is_empty())
                .or_else(|| p.cwd.filter(|c| !c.is_empty()))?;
            let tab = p.tab_id.filter(|t| !t.is_empty())?;
            Some((tab, PathBuf::from(cwd)))
        })
        .collect())
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

/// Whether `path` has any live session (a pane cwd'd at or below it). Used
/// for the bare `◉` row marker; the session names go in the context menu.
pub(crate) fn is_live(path: &Path, panes: &[(String, PathBuf)]) -> bool {
    panes.iter().any(|(_, p)| p.starts_with(path))
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
    fn is_live_matches_a_pane_inside_the_path() {
        let p = panes(&[("s", "/code/app/src")]);
        assert!(is_live(Path::new("/code/app"), &p));
        assert!(!is_live(Path::new("/code/app-sibling"), &p));
        assert!(!is_live(Path::new("/code/app"), &[]));
    }

    #[test]
    fn parse_herdr_panes_prefers_foreground_cwd_and_drops_null_tab() {
        let out = "{\"result\":{\"panes\":[
            {\"pane_id\":\"w1:p1\",\"tab_id\":\"w1:t1\",\"cwd\":\"/code/app\"},
            {\"pane_id\":\"w1:p2\",\"tab_id\":\"w1:t1\",\"cwd\":\"/code/app\",
             \"foreground_cwd\":\"/code/app/src\"},
            {\"pane_id\":\"w1:p3\",\"tab_id\":\"w1:t2\",\"cwd\":\"/elsewhere\"},
            {\"pane_id\":\"w1:p4\",\"tab_id\":null,\"cwd\":\"/code/app\"}
        ]}}";
        assert_eq!(
            parse_herdr_panes(out).unwrap(),
            vec![
                ("w1:t1".to_string(), PathBuf::from("/code/app")),
                ("w1:t1".to_string(), PathBuf::from("/code/app/src")),
                ("w1:t2".to_string(), PathBuf::from("/elsewhere")),
            ]
        );
    }

    #[test]
    fn parse_herdr_panes_skips_non_json_and_empty() {
        assert!(parse_herdr_panes("not json").is_err());
        // A missing `result` or `panes` (e.g. a future herdr renaming either
        // field) is an Err, so shape drift fails loudly instead of silently
        // showing "nothing live".
        assert!(parse_herdr_panes("{}").is_err());
        assert!(parse_herdr_panes("{\"result\":{}}").is_err());
        // A genuinely empty `panes: []` still parses to an empty set.
        assert!(
            parse_herdr_panes("{\"result\":{\"panes\":[]}}")
                .unwrap()
                .is_empty()
        );
        // A pane with a cwd but no tab id is dropped (nothing to focus).
        let out = "{\"result\":{\"panes\":[{\"pane_id\":\"w1:p1\",\"cwd\":\"/code\"}]}}";
        assert!(parse_herdr_panes(out).unwrap().is_empty());
    }

    #[test]
    fn parse_herdr_panes_falls_back_from_empty_foreground_cwd() {
        // An empty foreground_cwd is treated as absent: the label cwd wins.
        let out = "{\"result\":{\"panes\":[{\"pane_id\":\"w1:p1\",\"tab_id\":\"w1:t1\",
            \"cwd\":\"/code/app\",\"foreground_cwd\":\"\"}]}}";
        assert_eq!(
            parse_herdr_panes(out).unwrap(),
            vec![("w1:t1".to_string(), PathBuf::from("/code/app"))]
        );
    }
}
