//! Multiplexer detection: gitpane adapts its launchpad (`o`/`v`), its liveness
//! markers, and its session-attach to the terminal multiplexer it runs inside.
//! tmux is the original backend; herdr is supported natively (pane split /
//! tab create / tab focus) so the same dashboard verbs work there too.
//! The official desktop is detected separately: `HC_TERMINAL_ID` must resolve
//! to a displayed local terminal in `herdr-app ls`. Hidden core workers do not
//! imply an app pane. The verified source pane is pinned independently of focus.
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
//! Environment variables cannot identify the innermost owner on their own.
//! Legacy Herdr keeps its direct-parent/self tmux check. Official desktop
//! detection follows up to 64 process ancestors, so wrappers and editor workers
//! still resolve an inner tmux pane. The HC terminal root ends that walk before
//! a leaked outer tmux pane can claim it. Failed process probes fall back to the
//! displayed desktop context.

use std::sync::OnceLock;

/// The terminal multiplexer this instance runs under, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Multiplexer {
    /// tmux: `$TMUX` is set and we are (or a parent is) a tmux pane.
    Tmux,
    /// herdr: `$HERDR_*` is set and we are in a herdr pane.
    Herdr,
    /// Official herdr-app: the HC terminal is displayed in its local layout.
    HerdrApp,
    /// A plain terminal, or a multiplexer gitpane does not know.
    None,
}

impl Multiplexer {
    /// Detect from the real environment, resolving nested-tmux/herdr ambiguity
    /// (see the module docs). Successful detection is cached for the process.
    pub(crate) fn detect() -> Result<Self, String> {
        static CACHE: OnceLock<Multiplexer> = OnceLock::new();
        if let Some(mux) = CACHE.get() {
            return Ok(*mux);
        }
        let mux = detect_uncached()?;
        Ok(*CACHE.get_or_init(|| mux))
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

fn detect_uncached() -> Result<Multiplexer, String> {
    let mux = Multiplexer::detect_from(|v| std::env::var_os(v).is_some());
    let desktop_member = super::herdr_app::detected_context()?.is_some();
    let host = if desktop_member {
        Multiplexer::HerdrApp
    } else {
        mux
    };
    let nested = matches!(host, Multiplexer::Herdr | Multiplexer::HerdrApp)
        && (std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some());
    Ok(if nested && host == Multiplexer::HerdrApp {
        desktop_owner(
            &tmux_pane_pids(),
            &process_parents(),
            std::process::id(),
            parent_pid(),
            super::herdr_app::current_terminal_pid(),
        )
    } else if nested {
        resolve_host(host, &tmux_pane_pids(), parent_pid(), std::process::id())
    } else {
        host
    })
}

fn resolve_host(host: Multiplexer, panes: &[u32], parent: u32, self_pid: u32) -> Multiplexer {
    if disambiguate_nested(panes, parent, self_pid) == Multiplexer::Tmux {
        Multiplexer::Tmux
    } else {
        host
    }
}

/// Follow wrappers and extension workers, stopping at the desktop terminal root
/// before any inherited outer tmux pane can claim the process.
fn desktop_owner(
    panes: &[u32],
    parents: &[(u32, u32)],
    self_pid: u32,
    parent: u32,
    desktop_pid: Option<u32>,
) -> Multiplexer {
    let mut pid = self_pid;
    for _ in 0..64 {
        if Some(pid) == desktop_pid {
            return Multiplexer::HerdrApp;
        }
        if panes.contains(&pid) {
            return Multiplexer::Tmux;
        }
        let next = if pid == self_pid {
            Some(parent)
        } else {
            parents
                .iter()
                .find_map(|&(child, parent)| (child == pid).then_some(parent))
        };
        match next {
            Some(next) if next != 0 && next != pid => pid = next,
            _ => break,
        }
    }
    Multiplexer::HerdrApp
}

fn process_parents() -> Vec<(u32, u32)> {
    let mut command = std::process::Command::new("ps");
    command.args(["-e", "-o", "pid=,ppid="]);
    let Ok(output) = crate::session::liveness::bounded_output(&mut command) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect()
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
    let mut command = std::process::Command::new("tmux");
    command.args(["list-panes", "-a", "-F", "#{pane_pid}"]);
    let output = crate::session::liveness::bounded_output(&mut command);
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
        assert_eq!(
            Multiplexer::detect_from(present(&["HC_TERMINAL_ID"])),
            Multiplexer::None,
        );
        assert_eq!(
            Multiplexer::detect_from(present(&["HC_TERMINAL_ID", "TMUX"])),
            Multiplexer::Tmux,
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

    #[test]
    fn official_desktop_preserves_nested_tmux_ownership() {
        assert_eq!(
            resolve_host(Multiplexer::HerdrApp, &[100, 200], 200, 300),
            Multiplexer::Tmux,
        );
        assert_eq!(
            resolve_host(Multiplexer::HerdrApp, &[100, 200], 500, 200),
            Multiplexer::Tmux,
        );
        assert_eq!(
            resolve_host(Multiplexer::HerdrApp, &[100, 200], 500, 600),
            Multiplexer::HerdrApp,
        );
        assert_eq!(
            resolve_host(Multiplexer::HerdrApp, &[], 500, 600),
            Multiplexer::HerdrApp,
        );
    }

    #[test]
    fn desktop_nested_workers_follow_ancestry_without_crossing_the_terminal_root() {
        let parents = [(600, 500), (500, 400), (400, 300), (300, 200), (200, 100)];
        assert_eq!(
            desktop_owner(&[300], &parents, 700, 600, Some(100)),
            Multiplexer::Tmux
        );
        assert_eq!(
            desktop_owner(&[200], &parents, 700, 600, Some(400)),
            Multiplexer::HerdrApp
        );
        assert_eq!(
            desktop_owner(&[300], &parents, 300, 200, Some(100)),
            Multiplexer::Tmux
        );
        assert_eq!(
            desktop_owner(&[300], &parents, 300, 200, Some(300)),
            Multiplexer::HerdrApp
        );
        assert_eq!(
            desktop_owner(&[300], &[(600, 500), (500, 600)], 700, 600, None),
            Multiplexer::HerdrApp
        );
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
