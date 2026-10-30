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
    #[serde(default)]
    expected_base_ref: Option<String>,
}

#[derive(Serialize)]
struct Action {
    id: String,
    label: String,
    kind: &'static str,
    key: Option<String>,
    command: Option<String>,
    placement: String,
    review_base: Option<String>,
}

impl Action {
    fn fingerprint(&self) -> String {
        let data = serde_json::to_vec(self).expect("launcher action is serializable");
        format!("{:x}", Sha256::digest(data))
    }

    fn summary(&self) -> Value {
        json!({ "id": self.id, "label": self.label, "kind": self.kind, "key": self.key, "fingerprint": self.fingerprint() })
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
        review_base: None,
    }];
    if config
        .review
        .command
        .as_deref()
        .is_some_and(|command| !command.trim().is_empty())
    {
        result.push(Action {
            id: "review".into(),
            label: "Gitpane Review".into(),
            kind: "review",
            key: None,
            command: config.review.command.clone(),
            placement: config.review.placement.clone(),
            review_base: config.review.base.clone(),
        });
    }
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
                review_base: None,
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

fn review_base(action: &Action, repo: &Path) -> Result<Option<String>, BridgeError> {
    if action.kind != "review" {
        if !action
            .command
            .as_deref()
            .is_some_and(|command| command.contains("{base}"))
        {
            return Ok(None);
        }
        return Err(BridgeError::new(
            "invalid_request",
            "A review base is not available for this launcher.",
        ));
    }
    let repository = git2::Repository::open(repo)
        .map_err(|error| BridgeError::new("git_error", error.to_string()))?;
    crate::git::commit_files::resolve_review_base(&repository, action.review_base.as_deref())
        .map(Some)
        .ok_or_else(|| {
            BridgeError::new(
                "invalid_request",
                "No review base resolved; set [review] base in config",
            )
        })
}

fn selected_with_base(
    repo: &Path,
    id: &str,
    fingerprint: &str,
    expected: Option<&str>,
) -> Result<Action, BridgeError> {
    let mut action = selected(&load()?, id, fingerprint)?;
    let base = review_base(&action, repo)?;
    if base.as_deref() != expected {
        return Err(BridgeError::new(
            "stale_launcher",
            "The review base changed. Select the launcher again.",
        ));
    }
    action.review_base = base;
    Ok(action)
}

fn make_plan(action: &Action, repo: &Path) -> Result<Value, BridgeError> {
    let base = review_base(action, repo)?;
    let mut plan = make_plan_with_base(action, repo, base.as_deref())?;
    if action.kind == "review" {
        plan["base_ref"] = json!(base);
    }
    Ok(plan)
}

fn make_plan_with_base(
    action: &Action,
    repo: &Path,
    base: Option<&str>,
) -> Result<Value, BridgeError> {
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
            match launcher::plan(Some(command), "command", path, base, Multiplexer::None) {
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
            let script =
                launcher::integrated_shell_script(command.expect("command was checked"), base)
                    .map_err(|message| BridgeError::new("invalid_request", message))?;
            Ok(
                json!({ "kind": "shell", "script": script, "env": { "GITPANE_LAUNCH_PATH": path, "GITPANE_LAUNCH_BASE": base.unwrap_or("") }, "fingerprint": fingerprint }),
            )
        }
        "ask" => Ok(json!({ "kind": "ask", "fingerprint": fingerprint })),
        placement
            if placement.starts_with("split-window") || placement.starts_with("new-window") =>
        {
            match launcher::plan(command, placement, path, base, Multiplexer::Tmux) {
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
    let action = selected_with_base(
        &repo,
        &input.id,
        &input.fingerprint,
        input.expected_base_ref.as_deref(),
    )?;
    scope.repository(&repo)?;
    Ok(json!({ "valid": true, "base_ref": action.review_base }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_launchers_preserve_raw_configured_bases_and_quote_shell_data() {
        let directory = tempfile::tempdir().unwrap();
        git2::Repository::init(directory.path()).unwrap();
        let mut config = Config::default();
        config.review.command = Some("reviewer {base} {path}".into());
        config.review.placement = "command".into();
        for base in ["", "missing-ref", "topic $(touch unsafe); 'quoted'"] {
            config.review.base = Some(base.into());
            let action = actions(&config).remove(1);
            let plan = make_plan(&action, directory.path()).unwrap();
            assert_eq!(plan["base_ref"], base);
            assert_eq!(
                plan["argv"],
                json!(["reviewer", base, directory.path().to_str().unwrap()])
            );
        }
        config.review.base = None;
        assert_eq!(
            make_plan(&actions(&config).remove(1), directory.path())
                .unwrap_err()
                .code,
            "invalid_request"
        );
        config.review.command = Some("reviewer {path}".into());
        assert_eq!(
            make_plan(&actions(&config).remove(1), directory.path())
                .unwrap_err()
                .code,
            "invalid_request"
        );
        #[cfg(unix)]
        {
            config.review.base = Some("topic $(touch unsafe); 'quoted'".into());
            config.review.command = Some("printf '%s\\n' {base} {path}".into());
            config.review.placement = "inline".into();
            let plan = make_plan(&actions(&config).remove(1), directory.path()).unwrap();
            let output = std::process::Command::new("sh")
                .args(["-c", plan["script"].as_str().unwrap()])
                .env(
                    "GITPANE_LAUNCH_BASE",
                    plan["env"]["GITPANE_LAUNCH_BASE"].as_str().unwrap(),
                )
                .env(
                    "GITPANE_LAUNCH_PATH",
                    plan["env"]["GITPANE_LAUNCH_PATH"].as_str().unwrap(),
                )
                .current_dir(directory.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{}\n{}\n",
                    config.review.base.unwrap(),
                    directory.path().display()
                )
            );
            assert!(!directory.path().join("unsafe").exists());
        }
    }

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
