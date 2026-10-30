use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Action, BridgeError, Scope, selected_with_base};
use crate::bridge::changes::RepositoryIdentity;
use crate::session::env::Multiplexer;
use crate::session::launcher::LaunchPlan;
use crate::session::{launcher, liveness};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct ChoicesRequest {
    repo: PathBuf,
    id: String,
    fingerprint: String,
    backend: String,
    #[serde(default)]
    expected_base_ref: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct ExecuteRequest {
    repo: PathBuf,
    id: String,
    fingerprint: String,
    expected_repo_identity: String,
    backend: String,
    choice: String,
    context_token: String,
    #[serde(default)]
    expected_base_ref: Option<String>,
}

struct Choice {
    id: String,
    label: String,
    placement: String,
    token: String,
    workspace: Option<String>,
    existing_terminals: Vec<String>,
}

fn error(code: &'static str, message: impl Into<String>) -> BridgeError {
    BridgeError::new(code, message)
}

fn stale() -> BridgeError {
    error(
        "stale_target",
        "Repository, launcher or multiplexer target changed; choose it again.",
    )
}

fn command(program: &str, args: &[&str]) -> Result<std::process::Output, BridgeError> {
    let mut process = Command::new(program);
    process.args(args);
    let output = liveness::bounded_output(&mut process)
        .map_err(|e| error("session_error", format!("Could not inspect {program}: {e}")))?;
    if !output.status.success() {
        return Err(error(
            "session_error",
            format!(
                "Could not inspect {program}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output)
}

#[cfg(unix)]
fn socket_identity(path: &str) -> Result<String, BridgeError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = std::fs::metadata(path).map_err(|e| {
        error(
            "session_error",
            format!("Multiplexer socket is unavailable: {e}"),
        )
    })?;
    if !metadata.file_type().is_socket() {
        return Err(error("session_error", "Multiplexer socket is invalid."));
    }
    Ok(format!("{}:{}:{}", path, metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn socket_identity(_path: &str) -> Result<String, BridgeError> {
    Err(error(
        "unsupported",
        "Multiplexer launchers are not supported on this platform.",
    ))
}

fn valid_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn token(identity: &str, action: &Action, backend: &str, context: &str, placement: &str) -> String {
    let data = json!([identity, action.fingerprint(), backend, context, placement]);
    crate::hash::hex(
        Sha256::digest(serde_json::to_vec(&data).expect("context is serializable")).as_ref(),
    )
}

fn add_choice(
    out: &mut Vec<Choice>,
    identity: &str,
    action: &Action,
    backend: &str,
    context: &str,
    values: (String, String, String),
) {
    let (id, label, placement) = values;
    if out.iter().any(|choice| choice.id == id) {
        return;
    }
    out.push(Choice {
        id,
        label,
        token: token(identity, action, backend, context, &placement),
        placement,
        workspace: None,
        existing_terminals: Vec::new(),
    });
}

struct TmuxPane {
    context: String,
    socket: String,
    server_pid: String,
    pane: String,
    session: String,
    label: String,
}

const TMUX_FORMAT: &str = "#{pid}\t#{socket_path}\t#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{session_name}:#{window_index}.#{pane_index} #{window_name}";

fn parse_tmux_panes(output: &[u8]) -> Result<Vec<TmuxPane>, BridgeError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| error("session_error", "Invalid tmux response."))?;
    let mut panes = Vec::new();
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 7
            || !fields[0].bytes().all(|b| b.is_ascii_digit())
            || fields[0].is_empty()
            || !valid_id(fields[2], '$')
            || !valid_id(fields[3], '@')
            || !valid_id(fields[4], '%')
            || !fields[5].bytes().all(|b| b.is_ascii_digit())
            || fields[5].is_empty()
        {
            return Err(error("session_error", "Invalid tmux pane response."));
        }
        if panes.len() >= 128 {
            return Err(error(
                "session_error",
                "Too many tmux panes to choose safely.",
            ));
        }
        let socket = socket_identity(fields[1])?;
        let context = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            fields[0], socket, fields[2], fields[3], fields[4], fields[5]
        );
        panes.push(TmuxPane {
            context,
            socket: fields[1].into(),
            server_pid: fields[0].into(),
            pane: fields[4].into(),
            session: fields[2].into(),
            label: fields[6].into(),
        });
    }
    if panes.is_empty() {
        return Err(error("session_error", "No tmux panes are available."));
    }
    Ok(panes)
}

fn tmux_panes() -> Result<Vec<TmuxPane>, BridgeError> {
    let output = command("tmux", &["list-panes", "-a", "-F", TMUX_FORMAT])?;
    parse_tmux_panes(&output.stdout)
}

fn resolve_tmux_target(target: &str) -> Result<TmuxPane, BridgeError> {
    if target.is_empty() || target.starts_with('-') || target.chars().any(char::is_control) {
        return Err(error("invalid_request", "Invalid tmux target."));
    }
    let output = command(
        "tmux",
        &["display-message", "-p", "-t", target, TMUX_FORMAT],
    )?;
    let mut panes = parse_tmux_panes(&output.stdout)?;
    if panes.len() != 1 {
        return Err(error("session_error", "Ambiguous tmux target."));
    }
    Ok(panes.remove(0))
}

fn replace_target(placement: &str, target: &str) -> Result<String, BridgeError> {
    let mut words = placement
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let index = words
        .iter()
        .position(|word| word == "-t")
        .ok_or_else(|| error("invalid_request", "Launcher target is missing."))?;
    let slot = words
        .get_mut(index + 1)
        .ok_or_else(|| error("invalid_request", "Launcher target is missing."))?;
    *slot = target.to_owned();
    Ok(words.join(" "))
}

fn requested_target(placement: &str) -> Result<Option<&str>, BridgeError> {
    let words = placement.split_whitespace().collect::<Vec<_>>();
    let mut target = None;
    for (index, word) in words.iter().enumerate() {
        if *word == "-t" {
            if target.is_some() {
                return Err(error(
                    "unsupported_placement",
                    "Multiple multiplexer targets are not supported.",
                ));
            }
            target = Some(
                *words
                    .get(index + 1)
                    .ok_or_else(|| error("invalid_request", "Launcher target is missing."))?,
            );
        } else if word.starts_with("-t") {
            return Err(error(
                "unsupported_placement",
                "Use a separate -t argument for the launcher target.",
            ));
        }
    }
    Ok(target)
}

fn tmux_choices(action: &Action, identity: &str) -> Result<Vec<Choice>, BridgeError> {
    let panes = tmux_panes()?;
    let mut result = Vec::new();
    if action.placement.trim() == "ask" {
        for pane in &panes {
            let new = format!("new-window -t {}:", pane.session);
            add_choice(
                &mut result,
                identity,
                action,
                "tmux",
                &pane.context,
                (
                    format!("new:{}:{}", pane.session, pane.pane),
                    format!("New window in {}", pane.label),
                    new,
                ),
            );
            for (direction, label) in [("-h", "Right of"), ("-v", "Below")] {
                let placement = format!("split-window {direction} -t {}", pane.pane);
                add_choice(
                    &mut result,
                    identity,
                    action,
                    "tmux",
                    &pane.context,
                    (
                        format!("{}:{}:{}", direction, pane.session, pane.pane),
                        format!("{label} {}", pane.label),
                        placement,
                    ),
                );
            }
        }
        return Ok(result);
    }
    let configured = requested_target(&action.placement)?;
    let inherited = std::env::var("TMUX_PANE").ok();
    let target = configured.or(inherited.as_deref()).ok_or_else(|| {
        error(
            "unsupported_placement",
            "Choose a configured tmux target or launch VS Code from a tmux pane.",
        )
    })?;
    let resolved = resolve_tmux_target(target)?;
    let pane = panes
        .iter()
        .find(|pane| pane.context == resolved.context)
        .ok_or_else(stale)?;
    let explicitly_targeted = configured.is_some();
    if !explicitly_targeted {
        let context = std::env::var("TMUX").map_err(|_| {
            error(
                "unsupported_placement",
                "The inherited tmux server is unavailable.",
            )
        })?;
        let mut parts = context.rsplitn(3, ',');
        let _session_index = parts.next();
        let pid = parts.next();
        let socket = parts.next();
        if pid != Some(pane.server_pid.as_str()) || socket != Some(pane.socket.as_str()) {
            return Err(error(
                "stale_target",
                "The inherited tmux pane belongs to another server.",
            ));
        }
    }
    let resolved_target = if action.placement.trim_start().starts_with("new-window") {
        if explicitly_targeted && target.contains(':') && !target.ends_with(':') {
            return Err(error(
                "unsupported_placement",
                "New-window placement with a specific index cannot be pinned safely.",
            ));
        }
        format!("{}:", pane.session)
    } else {
        pane.pane.clone()
    };
    let placement = if explicitly_targeted {
        replace_target(&action.placement, &resolved_target)?
    } else {
        format!("{} -t {resolved_target}", action.placement)
    };
    add_choice(
        &mut result,
        identity,
        action,
        "tmux",
        &pane.context,
        (
            "configured".into(),
            format!("Configured target: {}", pane.label),
            placement,
        ),
    );
    Ok(result)
}

fn herdr_choices(action: &Action, identity: &str) -> Result<Vec<Choice>, BridgeError> {
    let socket_path = std::env::var("HERDR_SOCKET_PATH").map_err(|_| {
        error(
            "unsupported_placement",
            "Launch VS Code from a Herdr pane to use this launcher.",
        )
    })?;
    let socket = socket_identity(&socket_path)?;
    let inherited = std::env::var("HERDR_PANE_ID").ok();
    let configured = requested_target(&action.placement)?;
    let target = configured.or(inherited.as_deref()).ok_or_else(|| {
        error(
            "unsupported_placement",
            "A verified Herdr pane is required.",
        )
    })?;
    if target.is_empty()
        || target.len() > 128
        || target.starts_with('-')
        || target
            .chars()
            .any(|value| value.is_whitespace() || value.is_control())
        || target.contains(';')
    {
        return Err(error("invalid_request", "Invalid Herdr pane target."));
    }
    let output = command("herdr", &["pane", "list"])?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| error("session_error", "Invalid Herdr response."))?;
    let panes = parsed
        .pointer("/result/panes")
        .and_then(Value::as_array)
        .ok_or_else(|| error("session_error", "Invalid Herdr pane list."))?;
    let mut matching = panes
        .iter()
        .filter(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(target));
    let pane = matching.next().ok_or_else(stale)?;
    if matching.next().is_some() {
        return Err(error("session_error", "Ambiguous Herdr pane target."));
    }
    let terminal = pane
        .get("terminal_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "unsupported_placement",
                "Herdr did not provide a stable terminal identity.",
            )
        })?;
    let workspace = pane
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                "unsupported_placement",
                "Herdr did not provide a workspace identity.",
            )
        })?;
    if workspace.len() > 128
        || workspace.starts_with('-')
        || workspace
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
    {
        return Err(error(
            "session_error",
            "Herdr returned an invalid workspace identity.",
        ));
    }
    let context = format!("{socket}\0{target}\0{terminal}\0{workspace}");
    let existing_terminals =
        if action.placement.trim() == "ask" || action.placement.trim() == "new-window" {
            panes
                .iter()
                .map(|pane| {
                    pane.get("terminal_id")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            error(
                                "unsupported_placement",
                                "Herdr did not provide stable terminal identities.",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
    let mut result = Vec::new();
    if action.placement.trim() == "ask" {
        add_choice(
            &mut result,
            identity,
            action,
            "herdr",
            &context,
            (
                "new-tab".into(),
                "New tab in current workspace".into(),
                "new-window".into(),
            ),
        );
        result
            .last_mut()
            .expect("new-tab choice was added")
            .workspace = Some(workspace.into());
        result
            .last_mut()
            .expect("new-tab choice was added")
            .existing_terminals = existing_terminals.clone();
        for (direction, label) in [
            ("-h", "Right of current pane"),
            ("-v", "Below current pane"),
        ] {
            let placement = format!("split-window {direction} -t {target}");
            add_choice(
                &mut result,
                identity,
                action,
                "herdr",
                &context,
                (direction.into(), label.into(), placement),
            );
        }
    } else {
        let is_new_tab = action.placement.trim() == "new-window";
        let placement = if configured.is_some() || is_new_tab {
            action.placement.clone()
        } else {
            format!("{} -t {target}", action.placement)
        };
        add_choice(
            &mut result,
            identity,
            action,
            "herdr",
            &context,
            (
                "configured".into(),
                format!("Configured target: {target}"),
                placement,
            ),
        );
        if is_new_tab {
            result
                .last_mut()
                .expect("configured choice was added")
                .workspace = Some(workspace.into());
            result
                .last_mut()
                .expect("configured choice was added")
                .existing_terminals = existing_terminals;
        }
    }
    Ok(result)
}

fn collect(action: &Action, identity: &str, backend: &str) -> Result<Vec<Choice>, BridgeError> {
    if action.kind != "review"
        && action
            .command
            .as_deref()
            .is_some_and(|value| value.contains("{base}"))
    {
        return Err(error(
            "invalid_request",
            "A review base is not available for this launcher.",
        ));
    }
    if action.placement.trim() != "ask"
        && !matches!(
            action.placement.split_whitespace().next(),
            Some("split-window" | "new-window")
        )
    {
        return Err(error(
            "unsupported_placement",
            "This launcher does not use a multiplexer placement.",
        ));
    }
    match backend {
        "tmux" => tmux_choices(action, identity),
        "herdr" => herdr_choices(action, identity),
        _ => Err(error("invalid_request", "Unknown multiplexer backend.")),
    }
}

pub(in crate::bridge) fn choices(
    scope: &Scope,
    input: ChoicesRequest,
) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&repo)?;
    let fingerprint = identity.fingerprint()?;
    let action = selected_with_base(
        &repo,
        &input.id,
        &input.fingerprint,
        input.expected_base_ref.as_deref(),
    )?;
    let choices = collect(&action, &fingerprint, &input.backend)?;
    selected_with_base(
        &repo,
        &input.id,
        &input.fingerprint,
        input.expected_base_ref.as_deref(),
    )?;
    if scope.repository(&input.repo)? != repo || RepositoryIdentity::read(&repo)? != identity {
        return Err(stale());
    }
    Ok(
        json!({"repo_identity":fingerprint,"choices":choices.into_iter().map(|choice| json!({"id":choice.id,"label":choice.label,"context_token":choice.token})).collect::<Vec<_>>(), "notice": null }),
    )
}

fn launch(
    argv: &[String],
    repo: &Path,
    partial: bool,
) -> Result<std::process::Output, BridgeError> {
    let Some(program) = argv.first() else {
        return Err(error("invalid_request", "Empty launcher command."));
    };
    let mut process = Command::new(program);
    process.args(&argv[1..]).current_dir(repo);
    let output = liveness::bounded_output(&mut process).map_err(|e| {
        let code = if partial {
            "partial_completion"
        } else if matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ) {
            "session_error"
        } else {
            "outcome_unknown"
        };
        error(
            code,
            format!(
                "{}Launcher did not complete: {e}",
                if partial {
                    "Herdr pane was created. "
                } else {
                    ""
                }
            ),
        )
    })?;
    if !output.status.success() {
        return Err(error(
            if partial {
                "partial_completion"
            } else {
                "session_error"
            },
            format!(
                "{}Launcher failed: {}",
                if partial {
                    "Herdr pane was created. "
                } else {
                    ""
                },
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output)
}

fn verified_created_tab(
    output: &[u8],
    workspace: &str,
    existing_terminals: &[String],
) -> Result<String, BridgeError> {
    let partial = || {
        error(
            "partial_completion",
            "Herdr created a tab, but its workspace or pane identity could not be verified.",
        )
    };
    let response: Value = serde_json::from_slice(output).map_err(|_| partial())?;
    let result = response.get("result").ok_or_else(partial)?;
    if result.get("type").and_then(Value::as_str) != Some("tab_created")
        || result.pointer("/tab/workspace_id").and_then(Value::as_str) != Some(workspace)
        || result
            .pointer("/root_pane/workspace_id")
            .and_then(Value::as_str)
            != Some(workspace)
    {
        return Err(partial());
    }
    let pane = result
        .pointer("/root_pane/pane_id")
        .and_then(Value::as_str)
        .ok_or_else(partial)?;
    let tab = result
        .pointer("/tab/tab_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(partial)?;
    if result.pointer("/root_pane/tab_id").and_then(Value::as_str) != Some(tab) {
        return Err(partial());
    }
    let terminal = result
        .pointer("/root_pane/terminal_id")
        .and_then(Value::as_str)
        .ok_or_else(partial)?;
    if pane.is_empty()
        || pane.len() > 128
        || pane.starts_with('-')
        || pane
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
        || terminal.is_empty()
        || existing_terminals
            .iter()
            .any(|existing| existing == terminal)
    {
        return Err(partial());
    }
    let current = command("herdr", &["pane", "list"]).map_err(|_| partial())?;
    let current: Value = serde_json::from_slice(&current.stdout).map_err(|_| partial())?;
    let panes = current
        .pointer("/result/panes")
        .and_then(Value::as_array)
        .ok_or_else(partial)?;
    let mut matching = panes
        .iter()
        .filter(|value| value.get("pane_id").and_then(Value::as_str) == Some(pane));
    let actual = matching.next().ok_or_else(partial)?;
    if matching.next().is_some()
        || actual.get("workspace_id").and_then(Value::as_str) != Some(workspace)
        || actual.get("terminal_id").and_then(Value::as_str) != Some(terminal)
        || actual.get("tab_id").and_then(Value::as_str) != Some(tab)
    {
        return Err(partial());
    }
    Ok(pane.to_owned())
}

pub(in crate::bridge) fn execute(
    scope: &Scope,
    input: ExecuteRequest,
) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&repo)?;
    let fingerprint = identity.fingerprint()?;
    if fingerprint != input.expected_repo_identity {
        return Err(stale());
    }
    let action = selected_with_base(
        &repo,
        &input.id,
        &input.fingerprint,
        input.expected_base_ref.as_deref(),
    )?;
    let choices = collect(&action, &fingerprint, &input.backend)?;
    let choice = choices
        .iter()
        .find(|choice| choice.id == input.choice && choice.token == input.context_token)
        .ok_or_else(stale)?;
    let mux = match input.backend.as_str() {
        "tmux" => Multiplexer::Tmux,
        "herdr" => Multiplexer::Herdr,
        _ => return Err(error("invalid_request", "Unknown multiplexer backend.")),
    };
    let path = repo
        .to_str()
        .ok_or_else(|| error("invalid_request", "Repository path is not valid UTF-8."))?;
    let plan = if let Some(workspace) = &choice.workspace {
        launcher::plan_with_herdr_workspace(
            action.command.as_deref(),
            &choice.placement,
            path,
            action.review_base.as_deref(),
            workspace,
        )
    } else {
        launcher::plan(
            action.command.as_deref(),
            &choice.placement,
            path,
            action.review_base.as_deref(),
            mux,
        )
    };
    if scope.repository(&input.repo)? != repo || RepositoryIdentity::read(&repo)? != identity {
        return Err(stale());
    }
    selected_with_base(
        &repo,
        &input.id,
        &input.fingerprint,
        input.expected_base_ref.as_deref(),
    )?;
    match plan {
        LaunchPlan::Spawn(argv) => {
            launch(&argv, &repo, false)?;
        }
        LaunchPlan::Herdr { create, command } => {
            let output = launch(&create, &repo, false)?;
            let tab_pane = if let Some(workspace) = &choice.workspace {
                if !scope
                    .repository(&input.repo)
                    .is_ok_and(|current| current == repo)
                    || !RepositoryIdentity::read(&repo).is_ok_and(|current| current == identity)
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr created a tab but the repository changed.",
                    ));
                }
                if selected_with_base(
                    &repo,
                    &input.id,
                    &input.fingerprint,
                    input.expected_base_ref.as_deref(),
                )
                .is_err()
                    || !collect(&action, &fingerprint, &input.backend).is_ok_and(|values| {
                        values.iter().any(|value| {
                            value.id == input.choice && value.token == input.context_token
                        })
                    })
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr created a tab but the launcher context changed.",
                    ));
                }
                let pane =
                    verified_created_tab(&output.stdout, workspace, &choice.existing_terminals)?;
                if selected_with_base(
                    &repo,
                    &input.id,
                    &input.fingerprint,
                    input.expected_base_ref.as_deref(),
                )
                .is_err()
                    || !collect(&action, &fingerprint, &input.backend).is_ok_and(|values| {
                        values.iter().any(|value| {
                            value.id == input.choice && value.token == input.context_token
                        })
                    })
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr created a tab but the launcher context changed.",
                    ));
                }
                Some(pane)
            } else {
                None
            };
            if let Some(command) = command {
                let output = std::str::from_utf8(&output.stdout).map_err(|_| {
                    error(
                        "partial_completion",
                        "Herdr pane was created but its response was invalid.",
                    )
                })?;
                let pane = tab_pane
                    .or_else(|| launcher::parse_herdr_pane_id(output))
                    .ok_or_else(|| {
                        error(
                            "partial_completion",
                            "Herdr pane was created but its identity was unavailable.",
                        )
                    })?;
                if pane.is_empty() || pane.starts_with('-') || pane.chars().any(char::is_whitespace)
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr pane was created but its identity was invalid.",
                    ));
                }
                if !scope
                    .repository(&input.repo)
                    .is_ok_and(|current| current == repo)
                    || !RepositoryIdentity::read(&repo).is_ok_and(|current| current == identity)
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr pane was created but the repository changed before the command ran.",
                    ));
                }
                if selected_with_base(
                    &repo,
                    &input.id,
                    &input.fingerprint,
                    input.expected_base_ref.as_deref(),
                )
                .is_err()
                    || !collect(&action, &fingerprint, &input.backend).is_ok_and(|values| {
                        values.iter().any(|value| {
                            value.id == input.choice && value.token == input.context_token
                        })
                    })
                {
                    return Err(error(
                        "partial_completion",
                        "Herdr pane was created but the launcher context changed before the command ran.",
                    ));
                }
                launch(
                    &["herdr".into(), "pane".into(), "run".into(), pane, command],
                    &repo,
                    true,
                )?;
            }
        }
        LaunchPlan::Error(message) => return Err(error("invalid_request", message)),
        _ => {
            return Err(error(
                "unsupported_placement",
                "Launcher placement is not supported.",
            ));
        }
    }
    Ok(json!({"launched":true}))
}

#[cfg(all(test, unix))]
#[path = "mux_tests.rs"]
mod tests;
