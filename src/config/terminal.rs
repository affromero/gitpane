// ── Terminal "go to session" table ──────────────────────────────────────────
//
// `G` / "Attach" opens a repo's live tmux session in a new tab/window of the
// user's terminal. Each row maps the environment variable(s) that identify a
// terminal — they survive into tmux panes started by that terminal, so this
// works even though tmux overwrites `$TERM_PROGRAM` — to the command that opens
// it. Auto-detection is just a sensible default; `[goto] command` always wins.
//
// TO ADD A TERMINAL: append one `TerminalGoto` row below.
//   • `env`: any of these vars present ⇒ this terminal (first matching row wins).
//   • `command`: argv template, `{session}` ⇒ the tmux session name. It runs as
//     argv (NO shell), so pass the tmux command as TRAILING ARGS
//     (e.g. `… -- tmux attach -t {session}`), never a quoted string.
//   • Prefer a NEW TAB; use a NEW WINDOW only if the terminal's CLI can't run a
//     command in a tab (e.g. Ghostty). NEVER use `tmux switch-client` — gitpane
//     deliberately avoids an in-place switch, which strands you from gitpane.
//   • Make sure `launcher::goto_placement` recognizes the command so the menu
//     can label it "(new tab)"/"(new window)" — `test_goto_command_table` asserts
//     every row is classifiable.
// An unknown terminal yields "" ⇒ gitpane prompts the user to set `[goto]`.

pub(super) struct TerminalGoto {
    /// Env var(s) that identify the terminal (any present ⇒ match).
    pub(super) env: &'static [&'static str],
    /// argv template with a `{session}` placeholder.
    pub(super) command: &'static str,
}

#[cfg(target_os = "macos")]
pub(super) const GHOSTTY_GOTO: &str = "open -na Ghostty --args -e tmux attach -t {session}";
#[cfg(not(target_os = "macos"))]
pub(super) const GHOSTTY_GOTO: &str = "ghostty -e tmux attach -t {session}";

pub(super) const TERMINAL_GOTOS: &[TerminalGoto] = &[
    TerminalGoto {
        env: &["WEZTERM_PANE"],
        command: "wezterm cli spawn -- tmux attach -t {session}",
    },
    TerminalGoto {
        // Needs `allow_remote_control` in kitty.conf; best-effort default.
        env: &["KITTY_WINDOW_ID"],
        command: "kitten @ launch --type=tab tmux attach -t {session}",
    },
    TerminalGoto {
        env: &["GHOSTTY_RESOURCES_DIR"],
        command: GHOSTTY_GOTO,
    },
    TerminalGoto {
        env: &["KONSOLE_VERSION"],
        command: "konsole --new-tab -e tmux attach -t {session}",
    },
    TerminalGoto {
        // `msg create-window` talks over the IPC socket, so gate on the socket
        // (a bare window id doesn't mean IPC is available).
        env: &["ALACRITTY_SOCKET"],
        command: "alacritty msg create-window -e tmux attach -t {session}",
    },
    // Windows Terminal is intentionally omitted: `wt` from WSL needs a
    // distro-aware `cmd.exe /c wt.exe … wsl.exe -e …` form that varies per
    // setup, so WT users configure `[goto] command` explicitly.
    TerminalGoto {
        env: &["GNOME_TERMINAL_SCREEN"],
        command: "gnome-terminal --tab -- tmux attach -t {session}",
    },
];

/// Resolve the default `[goto] command` from the terminal table, using `present`
/// to test whether an env var is set. Empty when no terminal matches.
pub(super) fn goto_command_for_env(present: impl Fn(&str) -> bool) -> String {
    TERMINAL_GOTOS
        .iter()
        .find(|t| t.env.iter().any(|v| present(v)))
        .map(|t| t.command.to_string())
        .unwrap_or_default()
}

pub(super) fn default_goto_command() -> String {
    goto_command_for_env(|v| std::env::var_os(v).is_some())
}
