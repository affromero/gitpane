//! Multiplexer detection: gitpane adapts its launchpad (`o`/`v`), its liveness
//! markers, and its session-attach to the terminal multiplexer it runs inside.
//! tmux is the original backend; herdr is supported natively (pane split /
//! tab create / tab focus) so the same dashboard verbs work there too.
//!
//! # Nesting
//!
//! Neither multiplexer sanitizes the inherited environment, so a nested pane
//! carries *both* variable sets:
//!
//! - herdr sets `HERDR_*` on its panes and does not strip an inherited
//!   `TMUX`/`TMUX_PANE`, so a herdr pane inside tmux has both.
//! - tmux sets `TMUX`/`TMUX_PANE` for its panes and passes the server's
//!   inherited `HERDR_*` through, so a tmux pane inside herdr also has both.
//!
//! Environment variables therefore cannot tell which multiplexer owns the pane
//! when nested. [`Multiplexer::detect`] resolves the ambiguity by asking tmux
//! whether the process directly below us (our parent, or ourself when gitpane
//! was exec'd as the pane process) is one of its panes: if yes we are inside a
//! tmux pane; otherwise the pane is herdr's (the tmux env leaked in from an
//! ancestor). This holds for the normal launches — typing `gitpane` in the
//! pane's shell, `tmux new-session 'gitpane'`, `send-keys` — where the pane
//! shell is our direct parent. Launching through an extra wrapper (e.g.
//! `bash -c '...'`) puts the wrapper between us and the pane shell, and the
//! nested decision falls back to herdr; the `probe_detect` ignored test prints
//! the resolved values for manual checks. When tmux cannot
//! be asked (e.g. a dead tmux server) the nested case also falls back to
//! herdr.

use std::sync::OnceLock;

/// The terminal multiplexer this instance runs under, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Multiplexer {
    /// tmux: `$TMUX` is set and we are (or a parent is) a tmux pane.
    Tmux,
    /// herdr: `$HERDR_*` is set and we are in a herdr pane.
    Herdr,
    /// A plain terminal, or a multiplexer gitpane does not know.
    None,
}

impl Multiplexer {
    /// Detect from the real environment, resolving nested-tmux/herdr ambiguity
    /// (see the module docs). Cached for the process lifetime.
    pub(crate) fn detect() -> Self {
        static CACHE: OnceLock<Multiplexer> = OnceLock::new();
        *CACHE.get_or_init(detect_uncached)
    }

    /// Detect from `present`, which answers whether an env var is set. Pure env
    /// signal only: `HERDR_*` present means a herdr pane (a tmux pane inside
    /// herdr inherits them too), else `TMUX` means a tmux pane.
    pub(crate) fn detect_from(present: impl Fn(&str) -> bool) -> Self {
        if present("HERDR_ENV")
            || present("HERDR_PANE_ID")
            || present("HERDR_TAB_ID")
            || present("HERDR_WORKSPACE_ID")
        {
            Self::Herdr
        } else if present("TMUX") || present("TMUX_PANE") {
            Self::Tmux
        } else {
            Self::None
        }
    }
}

fn detect_uncached() -> Multiplexer {
    let mux = Multiplexer::detect_from(|v| std::env::var_os(v).is_some());
    if mux == Multiplexer::Herdr && std::env::var_os("TMUX").is_some() {
        return disambiguate_nested(&tmux_pane_pids(), parent_pid(), std::process::id());
    }
    mux
}

/// Nested case: if the process just below us (our parent, or ourself when
/// gitpane replaced the pane's shell) is a tmux pane process, we are inside a
/// tmux pane; otherwise the tmux env leaked in from an ancestor and the pane
/// is herdr's.
fn disambiguate_nested(tmux_pane_pids: &[u32], parent: u32, self_pid: u32) -> Multiplexer {
    if tmux_pane_pids.contains(&parent) || tmux_pane_pids.contains(&self_pid) {
        Multiplexer::Tmux
    } else {
        Multiplexer::Herdr
    }
}

/// PIDs of every pane process in the tmux server `$TMUX` points at. Empty when
/// tmux is absent, unreachable, or errors (the nested decision then falls back
/// to herdr).
fn tmux_pane_pids() -> Vec<u32> {
    let output = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_pid}"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Our parent PID. `libc::getppid()` is cross-platform on unix (Linux and
/// macOS); the previous `/proc/self/stat` parser only worked on Linux, so on
/// macOS the nested decision could never match and tmux-inside-herdr always
/// resolved to herdr. Non-unix platforms keep the old fallback of `0` (the
/// nested decision then falls back to herdr).
#[cfg(unix)]
fn parent_pid() -> u32 {
    // SAFETY: getppid() takes no arguments and never fails; it returns 0
    // only when the parent has already exited (the nested decision then
    // falls back to herdr, matching the old unreadable-proc behavior).
    unsafe { libc::getppid() as u32 }
}

#[cfg(not(unix))]
fn parent_pid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present<'a>(vars: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |v| vars.contains(&v)
    }

    #[test]
    fn plain_tmux_detects_tmux() {
        assert_eq!(
            Multiplexer::detect_from(present(&["TMUX"])),
            Multiplexer::Tmux
        );
        assert_eq!(
            Multiplexer::detect_from(present(&["TMUX", "TMUX_PANE"])),
            Multiplexer::Tmux
        );
    }

    #[test]
    fn plain_herdr_detects_herdr() {
        assert_eq!(
            Multiplexer::detect_from(present(&["HERDR_ENV"])),
            Multiplexer::Herdr
        );
        assert_eq!(
            Multiplexer::detect_from(present(&["HERDR_PANE_ID"])),
            Multiplexer::Herdr
        );
        assert_eq!(
            Multiplexer::detect_from(present(&["HERDR_TAB_ID", "HERDR_WORKSPACE_ID"])),
            Multiplexer::Herdr
        );
    }

    #[test]
    fn plain_terminal_is_none() {
        assert_eq!(Multiplexer::detect_from(present(&[])), Multiplexer::None);
        assert_eq!(
            Multiplexer::detect_from(present(&["WEZTERM_PANE"])),
            Multiplexer::None
        );
    }

    #[test]
    fn nested_tmux_pane_detects_tmux() {
        // herdr -> tmux -> gitpane: gitpane's parent (or self) is a tmux pane
        // process even though HERDR_* env is inherited alongside TMUX.
        assert_eq!(
            disambiguate_nested(&[100, 200, 300], 200, 400),
            Multiplexer::Tmux
        );
        // gitpane exec'd as the pane process itself.
        assert_eq!(
            disambiguate_nested(&[100, 200, 300], 0, 300),
            Multiplexer::Tmux
        );
    }

    #[test]
    fn nested_herdr_pane_detects_herdr() {
        // tmux -> herdr -> gitpane: the parent is a herdr pane shell, not a
        // tmux pane process — the tmux env leaked in from the ancestor.
        assert_eq!(
            disambiguate_nested(&[100, 200, 300], 500, 600),
            Multiplexer::Herdr
        );
        // Unreachable tmux (no pane pids) also falls back to herdr.
        assert_eq!(disambiguate_nested(&[], 500, 600), Multiplexer::Herdr);
    }

    #[cfg(unix)]
    #[test]
    fn parent_pid_is_nonzero() {
        // A running process always has a live parent (cargo during tests);
        // 0 would mean getppid failed or the parent exited.
        assert_ne!(parent_pid(), 0);
    }

    /// Manual environment probe (skipped by default): prints what
    /// [`Multiplexer::detect`] resolves in the *current* environment. Run it
    /// inside real nested setups to confirm the backend choice, e.g.
    /// `HERDR_ENV=1 TMUX=... cargo test -- --ignored probe_detect --nocapture`.
    #[test]
    #[ignore]
    fn probe_detect() {
        println!("detect() => {:?}", Multiplexer::detect());
        println!(
            "  self_pid={} parent_pid={} tmux_pane_pids={:?}",
            std::process::id(),
            parent_pid(),
            tmux_pane_pids(),
        );
    }
}
