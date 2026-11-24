//! Herdr Core terminals: the headless `herdr-core` runtime behind `hc`, used
//! from remote terminals and from machines with no Herdr desktop client.
//!
//! A core terminal is addressed by `$HC_TERMINAL_ID` and driven through the
//! `hc` CLI: `hc run --bg` opens a terminal, `hc ls` reports what is live.
//! Unlike the desktop backend there is no window manager — nothing can be
//! split, and no terminal can be focused from another one (attaching moves the
//! terminal you are already in), so a core placement always opens a new
//! terminal and focus reports why it cannot.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use super::herdr_app::error_detail;
use super::liveness::bounded_output;

/// Whether `value` is a local HC terminal address (`t` + alphanumerics).
pub(crate) fn valid_address(value: &str) -> bool {
    value.strip_prefix('t').is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

/// The core terminal this process runs in, when `$HC_TERMINAL_ID` names one.
pub(crate) fn terminal_id() -> Option<String> {
    std::env::var("HC_TERMINAL_ID")
        .ok()
        .filter(|value| valid_address(value))
}

/// `hc run --bg --outlive-parent --no-notify --cwd <dir> [sh -c <command>]` as
/// separate argv. The new terminal keeps its parent link (the app shows it
/// under the terminal it came from) but survives gitpane quitting, and nothing
/// is typed into the parent. No command opens the machine's own shell.
pub(crate) fn run_argv(dir: &str, command: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = [
        "hc",
        "run",
        "--bg",
        "--outlive-parent",
        "--no-notify",
        "--cwd",
    ]
    .iter()
    .map(|value| (*value).to_string())
    .collect();
    argv.push(dir.to_string());
    if let Some(command) = command {
        argv.extend([
            "--".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            command.to_string(),
        ]);
    }
    argv
}

/// Live core terminals as `(address, cwd)`, the address being what `hc attach`
/// takes. Foreign-machine terminals (`machine/t7`) are left out: their
/// directories do not describe this host.
pub(crate) fn pane_sessions() -> Result<Vec<(String, PathBuf)>, String> {
    let mut command = Command::new("hc");
    command.arg("ls");
    let output =
        bounded_output(&mut command).map_err(|error| format!("could not run hc: {error}"))?;
    if !output.status.success() {
        return Err(format!("hc ls failed: {}", error_detail(&output)));
    }
    parse_terminals(&output.stdout)
}

fn parse_terminals(output: &[u8]) -> Result<Vec<(String, PathBuf)>, String> {
    let value: Value =
        serde_json::from_slice(output).map_err(|error| format!("invalid hc response: {error}"))?;
    let terminals = value
        .get("terminals")
        .and_then(Value::as_array)
        .ok_or("invalid hc response: expected a terminals array")?;
    let mut panes = Vec::new();
    for terminal in terminals {
        let address = terminal
            .get("address")
            .and_then(Value::as_str)
            .ok_or("invalid hc response: terminal without an address")?;
        if !valid_address(address) {
            continue;
        }
        if terminal.get("state").and_then(Value::as_str) != Some("live") {
            continue;
        }
        let cwd = terminal
            .pointer("/foreground/cwd")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                terminal
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            });
        let Some(cwd) = cwd else {
            continue;
        };
        let path = PathBuf::from(cwd);
        if !path.is_absolute() {
            continue;
        }
        panes.push((address.to_string(), path));
    }
    Ok(panes)
}

/// Core terminals cannot be focused from another terminal: attach moves the
/// terminal you are already in. Say so instead of pretending to switch.
pub(crate) fn focus_error(address: &str) -> String {
    format!("herdr-core has no window focus; run `hc attach {address}` in a terminal")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(terminals: &str) -> Vec<u8> {
        format!("{{\"hidden_finished\":0,\"terminals\":[{terminals}]}}").into_bytes()
    }

    #[test]
    fn run_argv_opens_a_terminal_at_the_directory() {
        assert_eq!(
            run_argv("/code/app", None),
            vec![
                "hc",
                "run",
                "--bg",
                "--outlive-parent",
                "--no-notify",
                "--cwd",
                "/code/app",
            ]
        );
        // A configured command is run through `sh -c`, after `--`, so a
        // command that starts with a dash is program data, not a flag.
        assert_eq!(
            run_argv("/code/app", Some("git diff -U5 HEAD")),
            vec![
                "hc",
                "run",
                "--bg",
                "--outlive-parent",
                "--no-notify",
                "--cwd",
                "/code/app",
                "--",
                "sh",
                "-c",
                "git diff -U5 HEAD",
            ]
        );
    }

    #[test]
    fn parse_terminals_prefers_foreground_cwd_and_drops_finished_and_remote() {
        let out = ls(concat!(
            r#"{"address":"t8","state":"live","cwd":"/code/app","foreground":{"cwd":"/code/app/src"}},"#,
            r#"{"address":"t9","state":"finished","cwd":"/code/app"},"#,
            r#"{"address":"vps/t7","state":"live","cwd":"/code/app"},"#,
            r#"{"address":"t1b","state":"live","cwd":"/code/app"},{"address":"t1b","state":"live"}"#
        ));
        assert_eq!(
            parse_terminals(&out).unwrap(),
            vec![
                ("t8".to_string(), PathBuf::from("/code/app/src")),
                ("t1b".to_string(), PathBuf::from("/code/app")),
            ]
        );
    }

    #[test]
    fn parse_terminals_fails_loudly_on_shape_drift() {
        // A renamed or missing `terminals` array must not read as "nothing live".
        assert!(parse_terminals(b"{\"panes\":[]}").is_err());
        assert!(parse_terminals(b"not json").is_err());
        assert_eq!(parse_terminals(b"{\"terminals\":[]}").unwrap(), vec![]);
    }

    #[test]
    fn addresses_are_local_terminal_ids_only() {
        assert!(valid_address("t17"));
        assert!(valid_address("t1b"));
        assert!(!valid_address("t"));
        assert!(!valid_address("vps/t7"));
        assert!(!valid_address("t7 "));
    }
}
