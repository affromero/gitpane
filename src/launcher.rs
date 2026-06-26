//! Shared launcher for the `o` (open) and `v` (review) verbs. Given a command,
//! a placement, and a target directory, [`plan`] decides *how* to run it — as a
//! detached argv launcher, wrapped in a tmux pane/window, inlined into the
//! current terminal, or via an interactive picker — without touching any I/O,
//! so it is fully unit-testable. The caller executes the returned [`LaunchPlan`].

/// How a verb places the command it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Placement {
    /// The command itself is the full launcher, run detached as argv (no shell).
    /// An empty command opens a tmux pane (a shell) when inside tmux.
    Command,
    /// Wrap the command in `tmux <flags> -c <dir> sh -c <cmd>`. `flags[0]` is
    /// `split-window` or `new-window`; the rest are tmux flags (`-h`, `-t`, …).
    Tmux(Vec<String>),
    /// Suspend gitpane and run the command in the current terminal.
    Inline,
    /// Ask interactively where to place it.
    Ask,
}

/// What the caller must do to launch. Returned by [`plan`]; pure data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LaunchPlan {
    /// Spawn this argv detached (current_dir set by the caller).
    Spawn(Vec<String>),
    /// Run this `sh -c` string in the current terminal, suspending the TUI.
    Inline(String),
    /// Show the interactive placement picker.
    Ask,
    /// Surface this message via `Action::Error`; nothing was launched.
    Error(String),
}

/// POSIX single-quote a value for safe inclusion in a `sh -c` string: wrap in
/// `'…'` and rewrite each embedded `'` as `'\''`.
pub(crate) fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build argv from a command template by replacing every `{path}` token with
/// `dir` (one argv element per whitespace-split token, so the path is space-safe
/// without quoting). Used for `Placement::Command` (no shell).
fn substitute_argv(template: &str, dir: &str) -> Vec<String> {
    template
        .split_whitespace()
        .map(|tok| tok.replace("{path}", dir))
        .collect()
}

/// Substitute `{path}` and `{base}` into a template destined for `sh -c`,
/// shell-quoting each value so a path with spaces or a ref with shell
/// metacharacters cannot break out of the command.
fn substitute_shell(template: &str, dir: &str, base: Option<&str>) -> String {
    let mut s = template.replace("{path}", &shell_single_quote(dir));
    if let Some(b) = base {
        s = s.replace("{base}", &shell_single_quote(b));
    }
    s
}

/// Parse a placement string. `command`/`inline`/`ask` are keywords; anything
/// else must start with `split-window` or `new-window` (the rest are tmux
/// flags). Returns the error message for an unrecognized placement.
fn parse_placement(s: &str) -> Result<Placement, String> {
    let s = s.trim();
    match s {
        "command" => Ok(Placement::Command),
        "inline" => Ok(Placement::Inline),
        "ask" => Ok(Placement::Ask),
        _ => {
            let tokens: Vec<String> = s.split_whitespace().map(String::from).collect();
            match tokens.first().map(String::as_str) {
                Some("split-window") | Some("new-window") => Ok(Placement::Tmux(tokens)),
                _ => Err(format!(
                    "invalid placement '{s}'; use command, inline, ask, or split-window/new-window with flags"
                )),
            }
        }
    }
}

/// `tmux <flags> -c <dir> [sh -c <cmd>]` as separate argv (no double parsing).
fn build_tmux_argv(flags: &[String], dir: &str, cmd: Option<&str>) -> Vec<String> {
    let mut argv = vec!["tmux".to_string()];
    argv.extend(flags.iter().cloned());
    argv.push("-c".to_string());
    argv.push(dir.to_string());
    if let Some(c) = cmd {
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push(c.to_string());
    }
    argv
}

/// Decide how to launch `command` at `dir` under `placement`. `base` is the
/// review base ref (None for open). `in_tmux` is whether `$TMUX` is set. A tmux
/// placement (or `ask`) with no tmux falls back to running the command inline.
pub(crate) fn plan(
    command: Option<&str>,
    placement: &str,
    dir: &str,
    base: Option<&str>,
    in_tmux: bool,
) -> LaunchPlan {
    let placement = match parse_placement(placement) {
        Ok(p) => p,
        Err(e) => return LaunchPlan::Error(e),
    };
    let cmd = command.filter(|c| !c.trim().is_empty());
    match placement {
        Placement::Command => match cmd {
            Some(c) => LaunchPlan::Spawn(substitute_argv(c, dir)),
            None if in_tmux => LaunchPlan::Spawn(vec![
                "tmux".to_string(),
                "split-window".to_string(),
                "-c".to_string(),
                dir.to_string(),
            ]),
            None => LaunchPlan::Error("set a command or run gitpane inside tmux".to_string()),
        },
        Placement::Tmux(flags) => {
            let shell = cmd.map(|c| substitute_shell(c, dir, base));
            if in_tmux {
                LaunchPlan::Spawn(build_tmux_argv(&flags, dir, shell.as_deref()))
            } else if let Some(s) = shell {
                LaunchPlan::Inline(s)
            } else {
                LaunchPlan::Error("run gitpane inside tmux for this placement".to_string())
            }
        }
        Placement::Inline => match cmd {
            Some(c) => LaunchPlan::Inline(substitute_shell(c, dir, base)),
            None => LaunchPlan::Error("inline placement needs a command".to_string()),
        },
        Placement::Ask => {
            if in_tmux {
                LaunchPlan::Ask
            } else if let Some(c) = cmd {
                LaunchPlan::Inline(substitute_shell(c, dir, base))
            } else {
                LaunchPlan::Error("run gitpane inside tmux for this placement".to_string())
            }
        }
    }
}

/// Parse `tmux list-windows` output formatted as `<window_id>\t<label>` into
/// `(label, target)` pairs. The target is tmux's `window_id` (`@N`) — globally
/// unique and space-free, so a session name with spaces can't corrupt the
/// whitespace-split placement string. The label (session:index + name) is shown
/// in the picker. Lines without a tab are skipped.
pub(crate) fn parse_tmux_windows(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (target, label) = line.split_once('\t')?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            let label = label.trim();
            let label = if label.is_empty() {
                target.to_string()
            } else {
                label.to_string()
            };
            Some((label, target.to_string()))
        })
        .collect()
}

/// tmux windows across all sessions as `(label, target)`. Empty when tmux is
/// absent or errors.
pub(crate) fn tmux_windows() -> Vec<(String, String)> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-windows",
            "-a",
            "-F",
            "#{window_id}\t#{session_name}:#{window_index} #{window_name}",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => parse_tmux_windows(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Build placement-picker choices from tmux `windows`: "New window" plus
/// "Right of"/"Below" each window. Each entry is `(label, placement-string)`,
/// where the placement string is what `parse_placement`/`plan` consume.
pub(crate) fn placement_choices(windows: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = vec![("New window".to_string(), "new-window".to_string())];
    for (label, target) in windows {
        out.push((
            format!("Right of {label}"),
            format!("split-window -h -t {target}"),
        ));
        out.push((
            format!("Below {label}"),
            format!("split-window -v -t {target}"),
        ));
    }
    out
}

/// Build the argv for the `[goto] command`: whitespace-split, with every
/// `{session}` token replaced by `session` (one argv element per token, no
/// shell). Used to attach to a repo's live tmux session.
pub(crate) fn build_goto_argv(template: &str, session: &str) -> Vec<String> {
    template
        .split_whitespace()
        .map(|tok| tok.replace("{session}", session))
        .collect()
}

/// Short placement hint inferred from a `[goto] command`, for menu labels:
/// `Some("new tab")` / `Some("new window")` when the command opens one, else
/// `None` (a plain/unknown command).
pub(crate) fn goto_placement(command: &str) -> Option<&'static str> {
    if command.contains("cli spawn")        // wezterm
        || command.contains("--type=tab")   // kitty
        || command.contains("new-tab")      // wt / konsole --new-tab
        || command.contains("--tab")
    // gnome-terminal
    {
        Some("new tab")
    } else if command.contains("new-window")
        || command.contains("-na ")          // open -na (Ghostty mac)
        || command.starts_with("ghostty ")   // ghostty -e (Ghostty linux)
        || command.contains("create-window")
    // alacritty
    {
        Some("new window")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> LaunchPlan {
        LaunchPlan::Spawn(parts.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn goto_placement_infers_tab_or_window() {
        assert_eq!(
            goto_placement("wezterm cli spawn -- tmux attach -t {session}"),
            Some("new tab")
        );
        assert_eq!(
            goto_placement("kitten @ launch --type=tab tmux attach -t {session}"),
            Some("new tab")
        );
        assert_eq!(
            goto_placement("open -na Ghostty --args -e tmux attach -t {session}"),
            Some("new window")
        );
        assert_eq!(goto_placement("tmux switch-client -t {session}"), None);
    }

    #[test]
    fn goto_argv_substitutes_session() {
        assert_eq!(
            build_goto_argv("tmux switch-client -t {session}", "fairtrail"),
            vec!["tmux", "switch-client", "-t", "fairtrail"]
        );
        assert_eq!(
            build_goto_argv("wezterm cli spawn -- tmux attach -t {session}", "ft-rec"),
            vec![
                "wezterm", "cli", "spawn", "--", "tmux", "attach", "-t", "ft-rec"
            ]
        );
    }

    #[test]
    fn parse_keywords_and_tmux_and_invalid() {
        assert_eq!(parse_placement("command"), Ok(Placement::Command));
        assert_eq!(parse_placement("inline"), Ok(Placement::Inline));
        assert_eq!(parse_placement("ask"), Ok(Placement::Ask));
        assert_eq!(
            parse_placement("split-window -h -t agents"),
            Ok(Placement::Tmux(
                ["split-window", "-h", "-t", "agents"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            ))
        );
        assert!(parse_placement("kill-server").is_err());
        assert!(parse_placement("-t other").is_err());
    }

    #[test]
    fn command_mode_runs_detached_argv() {
        // open's default: the command is the launcher, run as argv (no shell).
        assert_eq!(
            plan(Some("cursor {path}"), "command", "/w t/app", None, false),
            argv(&["cursor", "/w t/app"])
        );
    }

    #[test]
    fn command_mode_empty_opens_tmux_pane_in_tmux() {
        assert_eq!(
            plan(None, "command", "/app", None, true),
            argv(&["tmux", "split-window", "-c", "/app"])
        );
    }

    #[test]
    fn command_mode_empty_without_tmux_errors() {
        assert!(matches!(
            plan(None, "command", "/app", None, false),
            LaunchPlan::Error(_)
        ));
    }

    #[test]
    fn tmux_placement_wraps_in_sh_c() {
        assert_eq!(
            plan(
                Some("git diff {base}...HEAD"),
                "new-window",
                "/app",
                Some("origin/main"),
                true
            ),
            argv(&[
                "tmux",
                "new-window",
                "-c",
                "/app",
                "sh",
                "-c",
                "git diff 'origin/main'...HEAD"
            ])
        );
    }

    #[test]
    fn tmux_placement_passes_flags_through() {
        assert_eq!(
            plan(
                Some("lazygit"),
                "split-window -h -t agents",
                "/app",
                None,
                true
            ),
            argv(&[
                "tmux",
                "split-window",
                "-h",
                "-t",
                "agents",
                "-c",
                "/app",
                "sh",
                "-c",
                "lazygit"
            ])
        );
    }

    #[test]
    fn tmux_placement_without_tmux_falls_back_to_inline() {
        assert_eq!(
            plan(
                Some("git diff {base}...HEAD | delta"),
                "new-window",
                "/app",
                Some("main"),
                false
            ),
            LaunchPlan::Inline("git diff 'main'...HEAD | delta".to_string())
        );
    }

    #[test]
    fn base_with_metacharacters_is_quoted() {
        assert_eq!(
            plan(
                Some("git diff {base}...HEAD"),
                "inline",
                "/app",
                Some("a;rm -rf b"),
                false
            ),
            LaunchPlan::Inline("git diff 'a;rm -rf b'...HEAD".to_string())
        );
    }

    #[test]
    fn ask_is_a_picker_in_tmux_and_inline_without() {
        assert_eq!(plan(Some("x"), "ask", "/app", None, true), LaunchPlan::Ask);
        assert_eq!(
            plan(Some("x"), "ask", "/app", None, false),
            LaunchPlan::Inline("x".to_string())
        );
    }

    #[test]
    fn command_mode_expands_embedded_path_token() {
        // `{path}` inside a token expands too (one argv element, space-safe).
        assert_eq!(
            plan(
                Some("wezterm cli spawn --cwd={path}"),
                "command",
                "/w t/x",
                None,
                false
            ),
            argv(&["wezterm", "cli", "spawn", "--cwd=/w t/x"])
        );
    }

    #[test]
    fn command_mode_blank_command_opens_tmux_pane() {
        // A whitespace-only command counts as empty.
        assert_eq!(
            plan(Some("   "), "command", "/repo", None, true),
            argv(&["tmux", "split-window", "-c", "/repo"])
        );
    }

    #[test]
    fn shell_mode_quotes_path_token() {
        // In shell modes, {path} is shell-quoted (it reaches `sh -c`).
        assert_eq!(
            plan(
                Some("cd {path} && git diff"),
                "inline",
                "/w t/x",
                None,
                false
            ),
            LaunchPlan::Inline("cd '/w t/x' && git diff".to_string())
        );
    }

    #[test]
    fn invalid_placement_is_an_error_plan() {
        assert!(matches!(
            plan(Some("x"), "frobnicate", "/app", None, true),
            LaunchPlan::Error(_)
        ));
    }

    #[test]
    fn parse_tmux_windows_skips_malformed_lines() {
        let out = "@0\tmain:0 editor\n@1\t\nno-tab-here\n@2\twork:2 logs\n";
        assert_eq!(
            parse_tmux_windows(out),
            vec![
                ("main:0 editor".to_string(), "@0".to_string()),
                ("@1".to_string(), "@1".to_string()), // empty label -> target as label
                ("work:2 logs".to_string(), "@2".to_string()),
            ]
        );
    }

    #[test]
    fn placement_choices_use_space_free_window_id_target() {
        // Even with a spaced label, the placement `-t` target is the window id,
        // so the whitespace-split placement string stays valid.
        let windows = vec![("my session:0 editor".to_string(), "@7".to_string())];
        assert_eq!(
            placement_choices(&windows),
            vec![
                ("New window".to_string(), "new-window".to_string()),
                (
                    "Right of my session:0 editor".to_string(),
                    "split-window -h -t @7".to_string()
                ),
                (
                    "Below my session:0 editor".to_string(),
                    "split-window -v -t @7".to_string()
                ),
            ]
        );
    }
}
