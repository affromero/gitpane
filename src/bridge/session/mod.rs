use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::session::{launcher, liveness};

use super::{BridgeError, scope::Scope};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListRequest {
    repo: PathBuf,
    backend: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GotoRequest {
    repo: PathBuf,
    backend: String,
    session: String,
}

fn sessions(repo: &Path, kind: &str) -> Result<Vec<String>, BridgeError> {
    let panes = liveness::checked_pane_sessions(kind)
        .map_err(|error| BridgeError::new("session_error", error.to_string()))?;
    Ok(liveness::live_sessions(repo, &panes))
}

pub(super) fn list(scope: &Scope, input: ListRequest) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let found = sessions(&repo, &input.backend)?;
    if scope.repository(&input.repo)? != repo {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while finding sessions",
        ));
    }
    Ok(json!({ "sessions": found }))
}

pub(super) fn goto(scope: &Scope, input: GotoRequest) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    if !sessions(&repo, &input.backend)?.contains(&input.session) {
        return Err(BridgeError::new(
            "stale_target",
            "Session no longer belongs to this repository",
        ));
    }
    let mut args = match input.backend.as_str() {
        "herdr" => vec![
            "herdr".to_owned(),
            "tab".to_owned(),
            "focus".to_owned(),
            input.session.clone(),
        ],
        "tmux" => {
            let config = crate::config::Config::load()
                .map_err(|error| BridgeError::new("session_error", error.to_string()))?;
            launcher::build_goto_argv(&config.goto.command, &input.session)
        }
        _ => {
            return Err(BridgeError::new(
                "invalid_request",
                "unknown session backend",
            ));
        }
    };
    if args.is_empty() {
        return Err(BridgeError::new(
            "session_error",
            "Set [goto] command for your terminal before opening a session",
        ));
    }
    if scope.repository(&input.repo)? != repo
        || !sessions(&repo, &input.backend)?.contains(&input.session)
    {
        return Err(BridgeError::new(
            "stale_target",
            "Session or repository changed before opening",
        ));
    }
    let executable = args.remove(0);
    let mut command = std::process::Command::new(executable);
    command.args(args).current_dir(repo);
    let output = liveness::bounded_output(&mut command).map_err(|error| {
        let code = if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::Unsupported
        ) {
            "session_error"
        } else {
            "outcome_unknown"
        };
        BridgeError::new(code, format!("Could not open session: {error}"))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BridgeError::new(
            "session_error",
            format!("Could not open session: {}", stderr.trim()),
        ));
    }
    Ok(json!({}))
}
