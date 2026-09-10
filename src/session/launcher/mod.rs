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

mod shell;

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

/// Expand placeholders in the original template only. Inserted paths and refs
/// may themselves contain placeholder text, which must remain literal data.
fn substitute_placeholders(template: &str, dir: &str, base: Option<&str>) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    for (offset, _) in template.match_indices('{') {
        let replacement = if template[offset..].starts_with("{path}") {
            Some(("{path}", dir))
        } else if template[offset..].starts_with("{base}") {
            base.map(|b| ("{base}", b))
        } else {
            None
        };
        if let Some((token, value)) = replacement {
            output.push_str(&template[cursor..offset]);
            output.push_str(value);
            cursor = offset + token.len();
        }
    }
    output.push_str(&template[cursor..]);
    output
}

/// Build argv with path/base substitution after whitespace splitting, keeping
/// each expanded value within its argument. No shell quoting is needed.
fn substitute_argv(template: &str, dir: &str, base: Option<&str>) -> Vec<String> {
    template
        .split_whitespace()
        .map(|tok| substitute_placeholders(tok, dir, base))
        .collect()
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
    plan_with_target(command, placement, dir, dir, base, mux)
}

/// Launch at `dir` while expanding `{path}` to `target`. Opening a file uses
/// its parent as the placement's working directory and the file as the target.
pub(crate) fn plan_with_target(
    command: Option<&str>,
    placement: &str,
    dir: &str,
    target: &str,
    base: Option<&str>,
    mux: Multiplexer,
) -> LaunchPlan {
    let dir = crate::repo_id::boundary_path(std::path::Path::new(dir));
    let target = crate::repo_id::boundary_path(std::path::Path::new(target));
    let dir = dir.to_string_lossy();
    let target = target.to_string_lossy();
    let (dir, target) = (dir.as_ref(), target.as_ref());
    let placement = match parse_placement(placement) {
        Ok(p) => p,
        Err(e) => return LaunchPlan::Error(e),
    };
    let cmd = command.filter(|c| !c.trim().is_empty());
    let shell = if matches!(placement, Placement::Command) {
        None
    } else {
        match cmd.map(|c| shell::substitute(c, target, base)).transpose() {
            Ok(command) => command,
            Err(error) => return LaunchPlan::Error(error),
        }
    };
    match placement {
        Placement::Command => match cmd {
            Some(c) => LaunchPlan::Spawn(substitute_argv(c, target, base)),
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
        Placement::Tmux(flags) => match mux {
            Multiplexer::Tmux => LaunchPlan::Spawn(build_tmux_argv(&flags, dir, shell.as_deref())),
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
                    LaunchPlan::Error("run gitpane inside tmux or herdr for this placement".into())
                }
            }
        },
        Placement::Inline => match shell {
            Some(command) => LaunchPlan::Inline(command),
            None => LaunchPlan::Error("inline placement needs a command".to_string()),
        },
        Placement::Ask => match mux {
            Multiplexer::Tmux | Multiplexer::Herdr => LaunchPlan::Ask,
            Multiplexer::None => {
                if let Some(command) = shell {
                    LaunchPlan::Inline(command)
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
    struct Payload {
        #[serde(default)]
        pane: Option<PaneId>,
        #[serde(default)]
        root_pane: Option<PaneId>,
    }
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(default)]
        result: Option<Payload>,
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
///
/// Opt-in only (`[herdr] forward_right_click`): herdr's CLI cannot read the
/// current routing back, so gitpane never restores it on exit — enabling this
/// intentionally leaves the pane forwarding right-click after gitpane quits.
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
mod tests;

#[cfg(all(test, unix))]
mod tests_shell;
