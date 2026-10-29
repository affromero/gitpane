use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{BridgeError, Scope};
use crate::config::Config;
use crate::session::env::Multiplexer;
use crate::session::launcher::{self, LaunchPlan};

pub(super) mod mux;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListRequest {
    repo: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionRequest {
    repo: PathBuf,
    id: String,
    fingerprint: String,
}

#[derive(Serialize)]
struct Action {
    id: String,
    label: String,
    kind: &'static str,
    key: Option<String>,
    command: Option<String>,
    placement: String,
}

impl Action {
    fn fingerprint(&self) -> String {
        let data = serde_json::to_vec(self).expect("launcher action is serializable");
        format!("{:x}", Sha256::digest(data))
    }

    fn summary(&self) -> Value {
        json!({ "id": self.id, "label": self.label, "kind": self.kind, "fingerprint": self.fingerprint() })
    }
}

fn actions(config: &Config) -> Vec<Action> {
    let mut result = vec![Action {
        id: "open".into(),
        label: "Gitpane Open".into(),
        kind: "open",
        key: None,
        command: config.open.command.clone(),
        placement: config.open.placement.clone(),
    }];
    result.extend(
        config
            .keybindings
            .iter()
            .enumerate()
            .map(|(index, binding)| Action {
                id: format!("key:{index}"),
                label: binding
                    .desc
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| format!("{}: {}", binding.key, binding.command)),
                kind: "keybinding",
                key: Some(binding.key.clone()),
                command: Some(binding.command.clone()),
                placement: binding.placement.clone(),
            }),
    );
    result
}

fn load() -> Result<Config, BridgeError> {
    Config::load().map_err(|error| BridgeError::new("config_error", error.to_string()))
}

fn selected(config: &Config, id: &str, fingerprint: &str) -> Result<Action, BridgeError> {
    let action = actions(config)
        .into_iter()
        .find(|action| action.id == id)
        .ok_or_else(|| {
            BridgeError::new(
                "stale_launcher",
                "The configured launcher no longer exists.",
            )
        })?;
    if fingerprint != action.fingerprint() {
        return Err(BridgeError::new(
            "stale_launcher",
            "The configured launcher changed. Select it again.",
        ));
    }
    Ok(action)
}

fn make_plan(action: &Action, repo: &Path) -> Result<Value, BridgeError> {
    let path = repo.to_str().ok_or_else(|| {
        BridgeError::new("invalid_request", "Repository path is not valid UTF-8.")
    })?;
    let command = action
        .command
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let fingerprint = action.fingerprint();
    match action.placement.trim() {
        "command" if command.is_none() => {
            Ok(json!({ "kind": "terminal", "fingerprint": fingerprint }))
        }
        "command" => {
            let command = command.expect("command was checked");
            if command.contains("{base}") {
                return Err(BridgeError::new(
                    "invalid_request",
                    "A review base is not available for this launcher.",
                ));
            }
            match launcher::plan(Some(command), "command", path, None, Multiplexer::None) {
                LaunchPlan::Spawn(argv) if !argv.is_empty() => {
                    Ok(json!({ "kind": "argv", "argv": argv, "fingerprint": fingerprint }))
                }
                LaunchPlan::Error(message) => Err(BridgeError::new("invalid_request", message)),
                _ => Err(BridgeError::new(
                    "invalid_request",
                    "The configured launcher cannot be opened in VS Code.",
                )),
            }
        }
        "inline" if command.is_some() => {
            if !cfg!(unix) {
                return Err(BridgeError::new(
                    "unsupported_placement",
                    "Inline shell launchers are not supported on this platform.",
                ));
            }
            let script = launcher::integrated_shell_script(command.expect("command was checked"))
                .map_err(|message| BridgeError::new("invalid_request", message))?;
            Ok(
                json!({ "kind": "shell", "script": script, "env": { "GITPANE_LAUNCH_PATH": path, "GITPANE_LAUNCH_BASE": "" }, "fingerprint": fingerprint }),
            )
        }
        "ask" => Ok(json!({ "kind": "ask", "fingerprint": fingerprint })),
        placement
            if placement.starts_with("split-window") || placement.starts_with("new-window") =>
        {
            if command.is_some_and(|value| value.contains("{base}")) {
                return Err(BridgeError::new(
                    "invalid_request",
                    "A review base is not available for this launcher.",
                ));
            }
            match launcher::plan(command, placement, path, None, Multiplexer::Tmux) {
                LaunchPlan::Spawn(_) => {
                    Ok(json!({ "kind": "multiplexer", "fingerprint": fingerprint }))
                }
                LaunchPlan::Error(message) => Err(BridgeError::new("invalid_request", message)),
                _ => Err(BridgeError::new(
                    "unsupported_placement",
                    "The configured launcher placement is not supported.",
                )),
            }
        }
        placement => Err(BridgeError::new(
            "unsupported_placement",
            format!("Launcher placement '{placement}' is not supported in VS Code."),
        )),
    }
}

pub(super) fn list(scope: &Scope, input: ListRequest) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let config = load()?;
    scope.repository(&repo)?;
    Ok(json!({ "actions": actions(&config).iter().map(Action::summary).collect::<Vec<_>>() }))
}

pub(super) fn plan(scope: &Scope, input: ActionRequest) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    let identity = super::changes::RepositoryIdentity::read(&repo)?;
    let action = selected(&load()?, &input.id, &input.fingerprint)?;
    let mut plan = make_plan(&action, &repo)?;
    if scope.repository(&repo)? != repo
        || super::changes::RepositoryIdentity::read(&repo)? != identity
    {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed; choose the launcher again.",
        ));
    }
    plan["repo_identity"] = json!(identity.fingerprint()?);
    Ok(plan)
}

pub(super) fn validate(scope: &Scope, input: ActionRequest) -> Result<Value, BridgeError> {
    let repo = scope.repository(&input.repo)?;
    selected(&load()?, &input.id, &input.fingerprint)?;
    scope.repository(&repo)?;
    Ok(json!({ "valid": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_launchers_keep_paths_as_data_and_reject_changes() {
        let mut config = Config::default();
        config.open.command = Some("editor {path}".into());
        let action = actions(&config).remove(0);
        let fingerprint = action.fingerprint();
        let repo = Path::new("/workspace/name $(touch bad)");
        let plan = make_plan(&action, repo).unwrap();
        assert_eq!(
            plan["argv"],
            json!(["editor", "/workspace/name $(touch bad)"])
        );
        config.open.command = Some("other {path}".into());
        assert_eq!(
            selected(&config, "open", &fingerprint).err().unwrap().code,
            "stale_launcher"
        );
        config.open.command = Some("printf '%s' {path}".into());
        config.open.placement = "inline".into();
        let action = actions(&config).remove(0);
        if cfg!(unix) {
            let plan = make_plan(&action, repo).unwrap();
            assert!(!plan["script"].as_str().unwrap().contains("$(touch bad)"));
            assert_eq!(plan["env"]["GITPANE_LAUNCH_PATH"], repo.to_str().unwrap());
        }
        config.open.placement = "new-window".into();
        assert_eq!(
            make_plan(&actions(&config).remove(0), repo).unwrap()["kind"],
            "multiplexer"
        );
    }
}
