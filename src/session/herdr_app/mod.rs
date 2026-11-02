//! Official Herdr desktop layout and herdr-core terminals. Terminal addresses
//! are resolved through the displayed layout; an HC terminal alone is not a pane.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{Value, json};

use super::liveness::bounded_output;

const MAX_PANES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub(crate) terminal: String,
    pub(crate) pane: String,
    pub(crate) tab: String,
    pub(crate) space: String,
}

#[derive(Deserialize)]
struct Layout {
    spaces: Vec<Space>,
}

#[derive(Deserialize)]
struct Space {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    machine: Option<String>,
    tabs: Vec<Tab>,
}

#[derive(Deserialize)]
struct Tab {
    id: String,
    #[serde(default)]
    name: Option<String>,
    terminals: Vec<Pane>,
}

#[derive(Deserialize)]
struct Pane {
    terminal: String,
    pane: String,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

fn local_terminal(value: &str) -> bool {
    value.strip_prefix('t').is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

fn parse_layout(output: &[u8]) -> Result<Layout, String> {
    let layout: Layout = serde_json::from_slice(output)
        .map_err(|error| format!("invalid herdr-app layout: {error}"))?;
    let mut ids = std::collections::HashSet::new();
    let mut terminals = std::collections::HashSet::new();
    let mut count = 0;
    for space in &layout.spaces {
        if !valid_id(&space.id) || !ids.insert(space.id.as_str()) {
            return Err("invalid or duplicate Herdr space identity".into());
        }
        for tab in &space.tabs {
            if !valid_id(&tab.id) || !ids.insert(tab.id.as_str()) {
                return Err("invalid or duplicate Herdr tab identity".into());
            }
            for pane in &tab.terminals {
                count += 1;
                if count > MAX_PANES {
                    return Err("too many Herdr panes to inspect safely".into());
                }
                if !valid_id(&pane.pane)
                    || !valid_id(&pane.terminal)
                    || !ids.insert(pane.pane.as_str())
                    || !terminals.insert(pane.terminal.as_str())
                {
                    return Err("invalid or duplicate Herdr pane identity".into());
                }
            }
        }
    }
    Ok(layout)
}

fn output_json(program: &str, args: &[&str]) -> Result<Value, String> {
    let mut command = Command::new(program);
    command.args(args);
    let output = bounded_output(&mut command)
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if !output.status.success() {
        return Err(command_failure(program, &output));
    }
    parse_response(program, &output.stdout)
}

fn parse_response(program: &str, output: &[u8]) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|error| format!("invalid {program} response: {error}"))?;
    if !value.is_object() {
        return Err(format!(
            "invalid {program} response: expected a JSON object"
        ));
    }
    if let Some(error) = value.get("error") {
        return Err(format!("{program} failed: {error}"));
    }
    Ok(value)
}

fn command_failure(program: &str, output: &std::process::Output) -> String {
    format!(
        "{program} failed ({}): {}",
        output.status,
        error_detail(output)
    )
}

pub(crate) fn error_detail(output: &std::process::Output) -> String {
    serde_json::from_slice::<Value>(&output.stdout)
        .as_ref()
        .ok()
        .and_then(|value| value.get("error"))
        .map(Value::to_string)
        .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_string())
}

fn layout() -> Result<Layout, String> {
    let mut command = Command::new("herdr-app");
    command.arg("ls");
    let output = bounded_output(&mut command)
        .map_err(|error| format!("could not inspect herdr-app: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "herdr-app ls failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_layout(&output.stdout)
}

type DisplayedPane<'a> = (&'a Space, &'a Tab, &'a Pane);

impl Layout {
    fn contexts(&self) -> impl Iterator<Item = DisplayedPane<'_>> {
        self.spaces
            .iter()
            .filter(|space| space.machine.is_none() && !space.id.contains('/'))
            .flat_map(|space| {
                space.tabs.iter().flat_map(move |tab| {
                    tab.terminals.iter().filter_map(move |pane| {
                        (local_terminal(&pane.terminal) && !pane.pane.contains('/'))
                            .then_some((space, tab, pane))
                    })
                })
            })
    }

    fn context(&self, terminal: &str) -> Result<Context, String> {
        let (space, tab, pane) = self
            .contexts()
            .find(|(_, _, pane)| pane.terminal == terminal)
            .ok_or_else(|| "terminal is not displayed in the local Herdr app".to_string())?;
        Ok(Context {
            terminal: terminal.into(),
            pane: pane.pane.clone(),
            tab: tab.id.clone(),
            space: space.id.clone(),
        })
    }

    fn verify(&self, context: &Context) -> Result<(), String> {
        if !self.contexts().any(|(space, tab, pane)| {
            pane.terminal == context.terminal
                && pane.pane == context.pane
                && tab.id == context.tab
                && space.id == context.space
        }) {
            return Err("Herdr source pane changed; restart gitpane in its current pane".into());
        }
        Ok(())
    }
}

/// Pin the containing pane when detection first succeeds, independently of focus.
pub(crate) fn current_context() -> Result<&'static Context, String> {
    static CONTEXT: LazyLock<Result<Context, String>> = LazyLock::new(|| {
        let terminal =
            std::env::var("HC_TERMINAL_ID").map_err(|_| "HC_TERMINAL_ID is not set".to_string())?;
        if !local_terminal(&terminal) {
            return Err("HC_TERMINAL_ID is not a local terminal address".into());
        }
        layout()?.context(&terminal)
    });
    CONTEXT.as_ref().map_err(Clone::clone)
}

/// The terminal's root process bounds nested-owner ancestry checks.
pub(crate) fn current_terminal_pid() -> Option<u32> {
    let context = current_context().ok()?;
    let value = output_json("hc", &["get", &context.terminal, "--json"]).ok()?;
    if value.get("address")?.as_str()? != context.terminal {
        return None;
    }
    value
        .get("pid")?
        .as_u64()?
        .try_into()
        .ok()
        .filter(|pid| *pid != 0)
}

#[derive(Debug, PartialEq, Eq)]
enum Placement<'a> {
    Tab(Option<&'a str>),
    Split {
        direction: &'static str,
        target: Option<&'a str>,
    },
}

fn placement(value: &str) -> Result<Placement<'_>, String> {
    let mut words = value.split_whitespace();
    let head = words.next().ok_or("empty herdr-app placement")?;
    let mut direction = "right";
    let mut target = None;
    while let Some(word) = words.next() {
        match word {
            "-h" if head == "split-window" => direction = "right",
            "-v" if head == "split-window" => direction = "down",
            "-t" if target.is_none() => {
                let id = words
                    .next()
                    .filter(|id| valid_id(id))
                    .ok_or("placement -t needs a valid Herdr target")?;
                target = Some(id);
            }
            _ => return Err(format!("unsupported herdr-app placement flag: {word}")),
        }
    }
    match head {
        "new-window" => Ok(Placement::Tab(target)),
        "split-window" => Ok(Placement::Split { direction, target }),
        _ => Err(format!("unsupported herdr-app placement: {head}")),
    }
}

fn resolve_target<'a>(
    layout: &'a Layout,
    source: &Context,
    value: &Placement<'_>,
) -> Result<DisplayedPane<'a>, String> {
    let selected = match value {
        Placement::Tab(None) | Placement::Split { target: None, .. } => layout
            .contexts()
            .find(|(_, _, pane)| pane.terminal == source.terminal),
        Placement::Tab(Some(target)) => layout.contexts().find(|(space, tab, pane)| {
            space.id == *target || tab.id == *target || pane.pane == *target
        }),
        Placement::Split {
            target: Some(target),
            ..
        } => layout.contexts().find(|(_, _, pane)| pane.pane == *target),
    };
    selected.ok_or_else(|| "Herdr target is no longer displayed locally".into())
}

pub(crate) fn validate_placement(value: &str) -> Result<(), String> {
    placement(value).map(|_| ())
}

/// The split API has no folder argument. Put cwd in argv as literal data and
/// use the same wrapper for tabs, preserving shell-template substitution rules.
pub(crate) fn program_argv(command: Option<&str>, dir: &str) -> Vec<String> {
    let script = match command {
        Some(command) => format!("cd \"$1\" && shift && {command}"),
        None => "cd \"$1\" && exec \"${SHELL:-/bin/sh}\" -l".into(),
    };
    vec![
        "sh".into(),
        "-c".into(),
        script,
        "gitpane".into(),
        dir.into(),
    ]
}

fn create_argv(
    layout: &Layout,
    context: &Context,
    value: &str,
    argv: &[String],
    dir: &Path,
) -> Result<Vec<String>, String> {
    layout.verify(context)?;
    if argv.is_empty() {
        return Err("empty Herdr terminal program".into());
    }
    let placement = placement(value)?;
    let (space, _, pane) = resolve_target(layout, context, &placement)?;
    let (command, args) = match placement {
        Placement::Tab(_) => (
            "tab.new",
            json!({ "space": space.id, "folder": dir, "argv": argv }),
        ),
        Placement::Split { direction, .. } => (
            "pane.split",
            json!({ "direction": direction, "pane": pane.pane, "argv": argv }),
        ),
    };
    Ok(vec![
        "herdr-app".into(),
        "do".into(),
        command.into(),
        args.to_string(),
    ])
}

pub(crate) fn prepare_launch(
    value: &str,
    argv: &[String],
    dir: &Path,
    context: &Context,
) -> Result<Vec<String>, String> {
    create_argv(&layout()?, context, value, argv, dir)
}

pub(crate) fn validate_launch_response(output: &[u8]) -> Result<(), String> {
    parse_response("herdr-app", output).map(|_| ())
}

pub(crate) fn launch(
    value: &str,
    argv: &[String],
    dir: &Path,
    context: &Context,
) -> Result<(), String> {
    let create = prepare_launch(value, argv, dir, context)?;
    let mut command = Command::new(&create[0]);
    command.args(&create[1..]).current_dir(dir);
    let output = bounded_output(&mut command)
        .map_err(|error| format!("Herdr launch outcome unknown: {error}"))?;
    if !output.status.success() {
        return Err(command_failure("herdr-app", &output));
    }
    validate_launch_response(&output.stdout)
}

fn choices(layout: &Layout, context: &Context) -> Result<Vec<(String, String)>, String> {
    layout.verify(context)?;
    let mut result = vec![(
        "New tab in current space".into(),
        format!("new-window -t {}", context.space),
    )];
    for (space, tab, pane) in layout
        .contexts()
        .filter(|(space, _, _)| space.id == context.space)
    {
        let label = format!(
            "{} / {} ({})",
            space.name.as_deref().unwrap_or(&space.id),
            tab.name.as_deref().unwrap_or(&tab.id),
            pane.pane
        );
        for (flag, direction) in [("-h", "Right of"), ("-v", "Below")] {
            result.push((
                format!("{direction} {label}"),
                format!("split-window {flag} -t {}", pane.pane),
            ));
        }
    }
    Ok(result)
}

pub(crate) fn placement_choices() -> Result<Vec<(String, String)>, String> {
    choices(&layout()?, current_context()?)
}

pub(crate) struct LauncherChoice {
    pub(crate) label: String,
    pub(crate) placement: String,
    pub(crate) identity: String,
}

pub(crate) fn launcher_choices(value: &str) -> Result<Vec<LauncherChoice>, String> {
    let layout = layout()?;
    let source = current_context()?;
    layout.verify(source)?;
    let values = if value.trim() == "ask" {
        choices(&layout, source)?
    } else {
        vec![("Configured Herdr target".into(), value.into())]
    };
    values
        .into_iter()
        .map(|(label, value)| {
            let placement = placement(&value)?;
            let (space, tab, pane) = resolve_target(&layout, source, &placement)?;
            Ok(LauncherChoice {
                label,
                placement: value,
                identity: format!(
                    "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                    source.terminal,
                    source.pane,
                    source.tab,
                    source.space,
                    pane.terminal,
                    pane.pane,
                    tab.id,
                    space.id
                ),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct Terminal {
    address: String,
    state: String,
    cwd: String,
    #[serde(default)]
    foreground: Option<Foreground>,
}

#[derive(Deserialize)]
struct Foreground {
    cwd: String,
}

fn terminal_cwd(value: Value, address: &str) -> Result<Option<PathBuf>, String> {
    let terminal: Terminal = serde_json::from_value(value)
        .map_err(|error| format!("invalid hc terminal response: {error}"))?;
    if terminal.address != address || !matches!(terminal.state.as_str(), "live" | "finished") {
        return Err("hc returned an unexpected terminal identity or state".into());
    }
    if terminal.state != "live" {
        return Ok(None);
    }
    let cwd = terminal
        .foreground
        .map(|foreground| foreground.cwd)
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(terminal.cwd);
    let path = PathBuf::from(cwd);
    if !path.is_absolute() {
        return Err("hc returned a non-absolute terminal directory".into());
    }
    Ok(Some(path))
}

/// Only displayed local terminals count; hidden workers and remote paths do not.
/// `hc ls` reports launch cwd, so get the foreground cwd for each displayed pane.
pub(crate) fn pane_sessions() -> Result<Vec<(String, PathBuf)>, String> {
    let layout = layout()?;
    let mut panes = Vec::new();
    for (_, _, pane) in layout.contexts() {
        let terminal = output_json("hc", &["get", &pane.terminal, "--json"])?;
        if let Some(cwd) = terminal_cwd(terminal, &pane.terminal)? {
            panes.push((pane.terminal.clone(), cwd));
        }
    }
    Ok(panes)
}

pub(crate) fn focus(terminal: &str) -> Result<(), String> {
    let layout = layout()?;
    if !layout
        .contexts()
        .any(|(_, _, pane)| pane.terminal == terminal)
    {
        return Err("terminal is not displayed in the local Herdr app".into());
    }
    output_json(
        "herdr-app",
        &[
            "do",
            "agent.focus",
            &json!({"terminal": terminal}).to_string(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
