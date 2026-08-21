//! Shared launcher for the `o` (open) and `v` (review) verbs. Given a command,
//! a placement, and a target directory, [`plan`] decides *how* to run it — as a
//! detached argv launcher, wrapped in a tmux pane/window, inlined into the
//! current terminal, or via an interactive picker — without touching any I/O,
//! so it is fully unit-testable. The caller executes the returned [`LaunchPlan`].
//!
//! The launch vocabulary is tmux-shaped (`split-window`/`new-window`); under
//! herdr ([`Multiplexer::Herdr`]) the same placements are translated to herdr's
//! `pane split` / `tab create` commands so config stays portable.

use crate::session::env::Multiplexer;

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
    /// herdr: run `create` (`herdr pane split` / `herdr tab create`), parse the
    /// new pane's id from its JSON response, then run `command` in that pane
    /// with `herdr pane run`. `command` is `None` for a bare shell pane.
    Herdr {
        create: Vec<String>,
        command: Option<String>,
    },
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
                Some("split-window") | Some("new-window") => {
                    // tmux parses `;` as a command separator, so a placement like
                    // "split-window ; kill-server" would chain extra tmux
                    // commands. Allow only split/new-window plus their flags.
                    if tokens.iter().any(|t| t.contains(';')) {
                        Err(format!(
                            "placement '{s}' may not contain ';' (a tmux command separator)"
                        ))
                    } else {
                        Ok(Placement::Tmux(tokens))
                    }
                }
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
/// review base ref (None for open). `mux` is the multiplexer this instance
/// runs under (see [`Multiplexer::detect`]). A tmux placement (or `ask`) with
/// no multiplexer falls back to running the command inline.
pub(crate) fn plan(
    command: Option<&str>,
    placement: &str,
    dir: &str,
    base: Option<&str>,
    mux: Multiplexer,
) -> LaunchPlan {
    let placement = match parse_placement(placement) {
        Ok(p) => p,
        Err(e) => return LaunchPlan::Error(e),
    };
    let cmd = command.filter(|c| !c.trim().is_empty());
    match placement {
        Placement::Command => match cmd {
            Some(c) => LaunchPlan::Spawn(substitute_argv(c, dir)),
            None => match mux {
                Multiplexer::Tmux => LaunchPlan::Spawn(vec![
                    "tmux".to_string(),
                    "split-window".to_string(),
                    "-c".to_string(),
                    dir.to_string(),
                ]),
                Multiplexer::Herdr => LaunchPlan::Herdr {
                    create: herdr_split_argv("right", dir, None),
                    command: None,
                },
                Multiplexer::None => {
                    LaunchPlan::Error("set a command or run gitpane inside tmux or herdr".into())
                }
            },
        },
        Placement::Tmux(flags) => {
            let shell = cmd.map(|c| substitute_shell(c, dir, base));
            match mux {
                Multiplexer::Tmux => {
                    LaunchPlan::Spawn(build_tmux_argv(&flags, dir, shell.as_deref()))
                }
                Multiplexer::Herdr => match herdr_create_argv(&flags, dir) {
                    Ok(create) => LaunchPlan::Herdr {
                        create,
                        command: shell,
                    },
                    Err(e) => LaunchPlan::Error(e),
                },
                Multiplexer::None => {
                    if let Some(s) = shell {
                        LaunchPlan::Inline(s)
                    } else {
                        LaunchPlan::Error(
                            "run gitpane inside tmux or herdr for this placement".into(),
                        )
                    }
                }
            }
        }
        Placement::Inline => match cmd {
            Some(c) => LaunchPlan::Inline(substitute_shell(c, dir, base)),
            None => LaunchPlan::Error("inline placement needs a command".to_string()),
        },
        Placement::Ask => match mux {
            Multiplexer::Tmux | Multiplexer::Herdr => LaunchPlan::Ask,
            Multiplexer::None => {
                if let Some(c) = cmd {
                    LaunchPlan::Inline(substitute_shell(c, dir, base))
                } else {
                    LaunchPlan::Error("run gitpane inside tmux or herdr for this placement".into())
                }
            }
        },
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

/// `herdr pane split --current --direction <right|down> --cwd <dir> --no-focus`,
/// with `--right-click pane` so a mouse TUI running in the new pane keeps its
/// right-click (herdr would otherwise swallow it with its own menu). `target`
/// replaces `--current` with `--pane <target>` when a `-t` placement flag named
/// a herdr pane id (e.g. `w1:p3`).
fn herdr_split_argv(direction: &str, dir: &str, target: Option<&str>) -> Vec<String> {
    let mut argv = vec!["herdr".to_string(), "pane".to_string(), "split".to_string()];
    match target {
        Some(t) => {
            argv.push("--pane".to_string());
            argv.push(t.to_string());
        }
        None => argv.push("--current".to_string()),
    }
    argv.push("--direction".to_string());
    argv.push(direction.to_string());
    argv.push("--cwd".to_string());
    argv.push(dir.to_string());
    argv.push("--no-focus".to_string());
    argv.push("--right-click".to_string());
    argv.push("pane".to_string());
    argv
}

/// `herdr tab create --cwd <dir> --no-focus` (a new tab, like tmux new-window).
fn herdr_tab_argv(dir: &str) -> Vec<String> {
    vec![
        "herdr".to_string(),
        "tab".to_string(),
        "create".to_string(),
        "--cwd".to_string(),
        dir.to_string(),
        "--no-focus".to_string(),
    ]
}

/// Translate tmux-style `split-window`/`new-window` flags into a herdr create
/// argv. `split-window` honors `-h`/`-v` (direction) and `-t <pane-id>`;
/// `new-window` takes no flags. Any other flag is an error so a tmux-specific
/// placement can't silently mis-launch under herdr.
fn herdr_create_argv(flags: &[String], dir: &str) -> Result<Vec<String>, String> {
    let mut rest = flags.iter();
    let Some(head) = rest.next() else {
        return Err("empty herdr placement".to_string());
    };
    match head.as_str() {
        "split-window" => {
            let mut direction = "right";
            let mut target = None;
            let mut extra = Vec::new();
            while let Some(tok) = rest.next() {
                match tok.as_str() {
                    "-h" => direction = "right",
                    "-v" => direction = "down",
                    "-t" => {
                        let t = rest.next().ok_or_else(|| {
                            "placement '-t' needs a target under herdr".to_string()
                        })?;
                        target = Some(t.clone());
                    }
                    other => extra.push(other.to_string()),
                }
            }
            if !extra.is_empty() {
                return Err(format!(
                    "placement flags {extra:?} are not supported under herdr (use -h, -v, -t <pane-id>)"
                ));
            }
            Ok(herdr_split_argv(direction, dir, target.as_deref()))
        }
        "new-window" if flags.len() == 1 => Ok(herdr_tab_argv(dir)),
        "new-window" => Err("placement 'new-window' takes no flags under herdr".to_string()),
        other => Err(format!("invalid placement '{other}' under herdr")),
    }
}

/// Placement-picker choices under herdr: a new tab, or split the current pane.
/// Each value is a tmux-shaped placement string that [`plan`] translates for
/// herdr, so the picker resume path stays multiplexer-agnostic.
pub(crate) fn herdr_placement_choices() -> Vec<(String, String)> {
    vec![
        ("New tab".to_string(), "new-window".to_string()),
        (
            "Right of current pane".to_string(),
            "split-window -h".to_string(),
        ),
        (
            "Below current pane".to_string(),
            "split-window -v".to_string(),
        ),
    ]
}

/// Extract the new pane id from a `herdr pane split` / `herdr tab create`
/// response: `.result.pane.pane_id` (split) or `.result.root_pane.pane_id`
/// (tab create). `None` when the output is not parseable herdr JSON.
pub(crate) fn parse_herdr_pane_id(output: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct PaneId {
        #[serde(default)]
        pane_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Result {
        #[serde(default)]
        pane: Option<PaneId>,
        #[serde(default)]
        root_pane: Option<PaneId>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(default)]
        result: Option<Result>,
    }
    let Ok(env) = serde_json::from_str::<Envelope>(output) else {
        return None;
    };
    let result = env.result?;
    if let Some(id) = result.pane.and_then(|p| p.pane_id) {
        return Some(id);
    }
    result.root_pane?.pane_id
}

/// Under herdr, forward right-click gestures to this pane so gitpane's context
/// menu works (herdr's own right-click menu would otherwise swallow them).
/// Best-effort and fire-and-forget: a missing herdr or server only logs at
/// debug. Right-clicking the pane frame still opens herdr's menu.
pub(crate) fn forward_right_click_in_herdr() {
    // Run whenever a herdr pane is reachable, not just when the pane we are in
    // is herdr's: in a tmux pane nested inside herdr, forwarding the ancestor
    // herdr pane lets the right-click reach tmux, which then passes it to us.
    let reachable = std::env::var_os("HERDR_ENV").is_some()
        || std::env::var_os("HERDR_PANE_ID").is_some()
        || std::env::var_os("HERDR_TAB_ID").is_some()
        || std::env::var_os("HERDR_WORKSPACE_ID").is_some();
    if !reachable {
        return;
    }
    let status = std::process::Command::new("herdr")
        .args(["pane", "input", "--current", "--right-click", "pane"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Err(e) = status {
        tracing::debug!("could not forward right-click to herdr: {e}");
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
        // A `;` would chain extra tmux commands — rejected even after a valid head.
        assert!(parse_placement("split-window -h ; kill-server").is_err());
        assert!(parse_placement("new-window;kill-server").is_err());
    }

    #[test]
    fn command_mode_runs_detached_argv() {
        // open's default: the command is the launcher, run as argv (no shell).
        assert_eq!(
            plan(
                Some("cursor {path}"),
                "command",
                "/w t/app",
                None,
                Multiplexer::None
            ),
            argv(&["cursor", "/w t/app"])
        );
    }

    #[test]
    fn command_mode_empty_opens_tmux_pane_in_tmux() {
        assert_eq!(
            plan(None, "command", "/app", None, Multiplexer::Tmux),
            argv(&["tmux", "split-window", "-c", "/app"])
        );
    }

    #[test]
    fn command_mode_empty_without_tmux_errors() {
        assert!(matches!(
            plan(None, "command", "/app", None, Multiplexer::None),
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
                Multiplexer::Tmux
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
                Multiplexer::Tmux
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
                Multiplexer::None
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
                Multiplexer::None
            ),
            LaunchPlan::Inline("git diff 'a;rm -rf b'...HEAD".to_string())
        );
    }

    #[test]
    fn ask_is_a_picker_in_tmux_and_inline_without() {
        assert_eq!(
            plan(Some("x"), "ask", "/app", None, Multiplexer::Tmux),
            LaunchPlan::Ask
        );
        assert_eq!(
            plan(Some("x"), "ask", "/app", None, Multiplexer::None),
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
                Multiplexer::None
            ),
            argv(&["wezterm", "cli", "spawn", "--cwd=/w t/x"])
        );
    }

    #[test]
    fn command_mode_blank_command_opens_tmux_pane() {
        // A whitespace-only command counts as empty.
        assert_eq!(
            plan(Some("   "), "command", "/repo", None, Multiplexer::Tmux),
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
                Multiplexer::None
            ),
            LaunchPlan::Inline("cd '/w t/x' && git diff".to_string())
        );
    }

    #[test]
    fn invalid_placement_is_an_error_plan() {
        assert!(matches!(
            plan(Some("x"), "frobnicate", "/app", None, Multiplexer::Tmux),
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

    fn herdr_argv(parts: &[&str]) -> LaunchPlan {
        LaunchPlan::Herdr {
            create: parts.iter().map(|s| s.to_string()).collect(),
            command: None,
        }
    }

    #[test]
    fn command_mode_empty_opens_herdr_pane_in_herdr() {
        assert_eq!(
            plan(None, "command", "/app", None, Multiplexer::Herdr),
            herdr_argv(&[
                "herdr",
                "pane",
                "split",
                "--current",
                "--direction",
                "right",
                "--cwd",
                "/app",
                "--no-focus",
                "--right-click",
                "pane",
            ])
        );
    }

    #[test]
    fn herdr_split_placement_honors_h_v_and_pane_target() {
        // `-h` -> right, `-v` -> down, `-t <pane-id>` -> `--pane`.
        assert_eq!(
            plan(
                Some("lazygit"),
                "split-window -h",
                "/app",
                None,
                Multiplexer::Herdr
            ),
            LaunchPlan::Herdr {
                create: vec![
                    "herdr",
                    "pane",
                    "split",
                    "--current",
                    "--direction",
                    "right",
                    "--cwd",
                    "/app",
                    "--no-focus",
                    "--right-click",
                    "pane",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                command: Some("lazygit".to_string()),
            }
        );
        let down = plan(
            Some("x"),
            "split-window -v",
            "/app",
            None,
            Multiplexer::Herdr,
        );
        assert!(matches!(
            down,
            LaunchPlan::Herdr {
                create: ref c,
                ..
            } if c.contains(&"down".to_string())
        ));
        let targeted = plan(
            Some("x"),
            "split-window -h -t w1:p3",
            "/app",
            None,
            Multiplexer::Herdr,
        );
        assert!(matches!(
            targeted,
            LaunchPlan::Herdr {
                create: ref c,
                ..
            } if c.contains(&"--pane".to_string()) && c.contains(&"w1:p3".to_string())
        ));
    }

    #[test]
    fn herdr_new_window_creates_a_tab() {
        // review's default `new-window` placement -> `herdr tab create`; the
        // command runs in the tab's root pane via `herdr pane run`.
        assert_eq!(
            plan(
                Some("git diff {base}...HEAD"),
                "new-window",
                "/app",
                Some("origin/main"),
                Multiplexer::Herdr,
            ),
            LaunchPlan::Herdr {
                create: vec!["herdr", "tab", "create", "--cwd", "/app", "--no-focus",]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                command: Some("git diff 'origin/main'...HEAD".to_string()),
            }
        );
    }

    #[test]
    fn herdr_rejects_unknown_and_tmux_only_flags() {
        // Unknown flags and `new-window` flags must not silently mis-launch.
        assert!(matches!(
            plan(
                Some("x"),
                "split-window -l 20",
                "/app",
                None,
                Multiplexer::Herdr
            ),
            LaunchPlan::Error(_)
        ));
        assert!(matches!(
            plan(
                Some("x"),
                "new-window -t work",
                "/app",
                None,
                Multiplexer::Herdr
            ),
            LaunchPlan::Error(_)
        ));
        assert!(matches!(
            plan(
                Some("x"),
                "split-window -t",
                "/app",
                None,
                Multiplexer::Herdr
            ),
            LaunchPlan::Error(_)
        ));
    }

    #[test]
    fn ask_is_a_picker_under_herdr() {
        assert_eq!(
            plan(Some("x"), "ask", "/app", None, Multiplexer::Herdr),
            LaunchPlan::Ask
        );
    }

    #[test]
    fn herdr_placement_choices_offer_tab_and_splits() {
        assert_eq!(
            herdr_placement_choices(),
            vec![
                ("New tab".to_string(), "new-window".to_string()),
                (
                    "Right of current pane".to_string(),
                    "split-window -h".to_string(),
                ),
                (
                    "Below current pane".to_string(),
                    "split-window -v".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn parse_herdr_pane_id_reads_split_and_tab_responses() {
        let split = "{\"id\":\"cli:pane:split\",\"result\":{\"pane\":{\"pane_id\":\"w1:p3\"}}}";
        assert_eq!(parse_herdr_pane_id(split), Some("w1:p3".to_string()));
        let tab =
            "{\"result\":{\"tab\":{\"tab_id\":\"w1:t2\"},\"root_pane\":{\"pane_id\":\"w1:p7\"}}}";
        assert_eq!(parse_herdr_pane_id(tab), Some("w1:p7".to_string()));
        assert_eq!(parse_herdr_pane_id("not json"), None);
    }
}
