//! Herdr Core terminals: the headless `herdr-core` runtime behind `hc`, used
//! from remote terminals and from machines with no Herdr desktop client.
//!
//! A core terminal is addressed by `$HC_TERMINAL_ID`. `hc run --bg` opens one,
//! `hc ls` reports what is live, and `hc get` describes one terminal. Unlike
//! the desktop backend no window manager is involved, so a core placement
//! always opens a new terminal rather than a split, and focus reports why it
//! cannot move one.
//!
//! # Opting into the window
//!
//! A terminal `hc` creates stays hidden until it carries `herdr.space`: the
//! window arranges terminals in spaces and tabs, and an unlabelled terminal is
//! a background worker it never shows. The calling terminal's own
//! `herdr.space` label therefore decides where a new one appears — the same
//! space, as a new tab.
//!
//! That label alone is not proof the source is usable. The environment is
//! inherited and outlives the terminal, so `$HC_TERMINAL_ID` may still name a
//! terminal that has finished; only `hc get` can confirm it is live.

use std::path::{Path, PathBuf};
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

/// A space or tab identity the window can accept as a label value: non-empty,
/// bounded, and free of the whitespace and control characters that would make
/// an ambiguous `--label` argument.
fn valid_label_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

/// The core terminal this process runs in, when `$HC_TERMINAL_ID` names one.
pub(crate) fn terminal_id() -> Option<String> {
    std::env::var("HC_TERMINAL_ID")
        .ok()
        .filter(|value| valid_address(value))
}

/// The space this process's terminal belongs to, read from its own labels.
///
/// Fails rather than guessing: a caller with no terminal, a terminal that has
/// finished, and a terminal that never opted into the window are all reported,
/// because each needs the person to do something different. Only a query that
/// could not run at all leaves the space unknown — that is an error too, never
/// a silent "no space".
pub(crate) fn source_space() -> Result<String, String> {
    let terminal = terminal_id().ok_or_else(|| {
        "no Herdr terminal here: run gitpane inside a Herdr-managed terminal, \
         or set [open] placement = \"inline\""
            .to_string()
    })?;
    let mut command = Command::new("hc");
    command.args(["get", &terminal, "--json"]);
    let output = bounded_output(&mut command)
        .map_err(|error| format!("could not inspect the Herdr terminal: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not inspect the Herdr terminal: {}",
            error_detail(&output)
        ));
    }
    parse_source_space(&output.stdout, &terminal)
}

fn parse_source_space(output: &[u8], terminal: &str) -> Result<String, String> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|error| format!("invalid hc terminal response: {error}"))?;
    if value.get("address").and_then(Value::as_str) != Some(terminal) {
        return Err("hc described a different terminal than the one asked for".into());
    }
    match value.get("state").and_then(Value::as_str) {
        Some("live") => {}
        Some(state) => {
            return Err(format!(
                "the terminal gitpane runs in is {state}, not live; \
                 restart gitpane in a current terminal"
            ));
        }
        None => return Err("invalid hc terminal response: missing state".into()),
    }
    let space = value
        .get("labels")
        .and_then(Value::as_object)
        .and_then(|labels| labels.get("herdr.space"))
        .and_then(Value::as_str);
    match space {
        Some(space) if valid_label_value(space) => Ok(space.to_string()),
        Some(_) => Err("this terminal's herdr.space label is not a usable id".into()),
        None => Err(
            "this terminal is not displayed in the Herdr window, so there is no space to \
             open a tab in; run gitpane in a Herdr-managed terminal, or set \
             [open] placement = \"inline\""
                .into(),
        ),
    }
}

/// `hc run --bg --outlive-parent --no-notify --cwd <dir> --label
/// herdr.space=<space> [-- sh -c <command>]` as separate argv.
///
/// The new terminal keeps its parent link (the window shows it under the
/// terminal it came from) but survives gitpane quitting, and nothing is typed
/// into the parent. The label is what makes it visible at all; it precedes the
/// `--` so it stays an option, while the command after `--` stays program data
/// even when it starts with a dash. No command opens the machine's own shell.
pub(crate) fn run_argv(dir: &str, command: Option<&str>, space: &str) -> Vec<String> {
    let mut argv: Vec<String> = [
        "hc",
        "run",
        "--json",
        "--bg",
        "--outlive-parent",
        "--no-notify",
        "--cwd",
    ]
    .iter()
    .map(|value| (*value).to_string())
    .collect();
    argv.push(dir.to_string());
    argv.push("--label".to_string());
    argv.push(format!("herdr.space={space}"));
    if let Some(command) = command {
        argv.extend(
            ["--", "sh", "-c", command]
                .iter()
                .map(|value| (*value).to_string()),
        );
    }
    argv
}

/// What a create request settled: a terminal, a refusal, or a create that may
/// or may not have happened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Created {
    /// The core reported this terminal.
    Terminal(String),
    /// The core refused, so nothing was created and retrying is safe.
    Refused(String),
    /// The request was sent but its result could not be read. A retry could
    /// open a second terminal, so the caller must check `hc ls` instead.
    Unknown(String),
}

impl Created {
    /// The message for a person, and whether a retry is safe.
    pub(crate) fn into_message(self) -> (String, bool) {
        match self {
            Created::Terminal(address) => (format!("opened terminal {address}"), false),
            Created::Refused(detail) => (detail, true),
            Created::Unknown(detail) => (detail, false),
        }
    }
}

/// Run `hc run`, waiting only for the create request, and report what it
/// settled.
///
/// The wait covers the request, not the program the terminal runs: `--bg`
/// returns as soon as the terminal exists.
pub(crate) fn launch(argv: &[String], dir: &Path) -> Created {
    let Some(program) = argv.first() else {
        return Created::Refused("empty Herdr launch command".into());
    };
    let mut command = Command::new(program);
    command.args(&argv[1..]).current_dir(dir);
    let output = match bounded_output(&mut command) {
        Ok(output) => output,
        Err(error) => {
            return Created::Unknown(format!(
                "the terminal may have opened, but the reply was lost ({error}); check `hc ls`"
            ));
        }
    };
    if !output.status.success() {
        return Created::Refused(format!(
            "could not open the terminal: {}",
            error_detail(&output)
        ));
    }
    classify_created(&output.stdout)
}

fn classify_created(output: &[u8]) -> Created {
    let unknown = |detail: String| {
        Created::Unknown(format!(
            "the terminal may have opened, but {detail}; check `hc ls`"
        ))
    };
    let value: Value = match serde_json::from_slice(output) {
        Ok(value) => value,
        Err(error) => return unknown(format!("its reply was {error}")),
    };
    if let Some(error) = value.get("error") {
        return Created::Refused(format!("could not open the terminal: {error}"));
    }
    match value
        .get("address")
        .and_then(Value::as_str)
        .filter(|address| valid_address(address))
    {
        Some(address) => Created::Terminal(address.to_string()),
        None => unknown("no terminal address was reported".into()),
    }
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

    fn terminal(state: &str, labels: &str) -> Vec<u8> {
        format!("{{\"address\":\"t17\",\"state\":\"{state}\",\"labels\":{{{labels}}}}}")
            .into_bytes()
    }

    #[test]
    fn run_argv_opens_a_visible_terminal_at_the_directory() {
        assert_eq!(
            run_argv("/code/app", None, "spc1"),
            vec![
                "hc",
                "run",
                "--json",
                "--bg",
                "--outlive-parent",
                "--no-notify",
                "--cwd",
                "/code/app",
                "--label",
                "herdr.space=spc1",
            ]
        );
        // The label must stay an option and the command must stay data: it sits
        // after `--`, so a command starting with a dash is not read as a flag.
        assert_eq!(
            run_argv("/code/app", Some("git diff -U5 HEAD"), "spc1"),
            vec![
                "hc",
                "run",
                "--json",
                "--bg",
                "--outlive-parent",
                "--no-notify",
                "--cwd",
                "/code/app",
                "--label",
                "herdr.space=spc1",
                "--",
                "sh",
                "-c",
                "git diff -U5 HEAD",
            ]
        );
    }

    #[test]
    fn source_space_reads_the_live_terminals_own_label() {
        let out = terminal(
            "live",
            "\"herdr.space\":\"spc104fd\",\"where\":\"repo / tab\"",
        );
        assert_eq!(parse_source_space(&out, "t17").unwrap(), "spc104fd");
    }

    #[test]
    fn source_space_refuses_a_terminal_that_already_finished() {
        // The environment outlives the terminal, so a valid id and a leftover
        // label prove nothing: only a live terminal can place a new tab.
        let out = terminal("finished", "\"herdr.space\":\"spc104fd\"");
        let error = parse_source_space(&out, "t17").unwrap_err();
        assert!(error.contains("finished"), "{error}");
        assert!(error.contains("restart gitpane"), "{error}");
    }

    #[test]
    fn source_space_separates_a_missing_label_from_a_broken_reply() {
        let hidden = terminal("live", "\"where\":\"repo / tab\"");
        let error = parse_source_space(&hidden, "t17").unwrap_err();
        assert!(error.contains("no space to open a tab in"), "{error}");
        assert!(error.contains("inline"), "{error}");

        // A reply about another terminal, a missing state and bad JSON are
        // backend failures, never "this terminal has no space".
        let other = terminal("live", "\"herdr.space\":\"spc1\"");
        assert!(
            parse_source_space(&other, "t99")
                .unwrap_err()
                .contains("different terminal")
        );
        let stateless = br#"{"address":"t17","labels":{"herdr.space":"spc1"}}"#;
        assert!(
            parse_source_space(stateless, "t17")
                .unwrap_err()
                .contains("missing state")
        );
        assert!(
            parse_source_space(b"not json", "t17")
                .unwrap_err()
                .contains("invalid hc")
        );
        // A label the window could not accept is not usable either.
        let padded = br#"{"address":"t17","state":"live","labels":{"herdr.space":"spc 1"}}"#;
        assert!(
            parse_source_space(padded, "t17")
                .unwrap_err()
                .contains("not a usable id")
        );
    }

    #[test]
    fn created_terminals_are_split_into_opened_refused_and_unknown() {
        assert_eq!(
            classify_created(br#"{"address":"t1x","state":"live"}"#),
            Created::Terminal("t1x".into())
        );
        // `hc` reporting its own refusal means nothing was created, so a retry
        // is safe; this must not read as an unknown outcome.
        let refused = classify_created(br#"{"error":{"kind":"spawn_failed"}}"#);
        assert!(
            matches!(&refused, Created::Refused(m) if m.contains("could not open")),
            "{refused:?}"
        );
        assert!(refused.into_message().1, "a refusal is safe to retry");
        // A reply that cannot be read may still describe a terminal that was
        // created, so it sends the person to `hc ls`, not to a retry.
        for reply in [
            &b"not json"[..],
            br#"{"state":"live"}"#,
            br#"{"address":"not-a-terminal"}"#,
        ] {
            let created = classify_created(reply);
            assert!(
                matches!(&created, Created::Unknown(m) if m.contains("check `hc ls`")),
                "{created:?}"
            );
            assert!(
                !created.into_message().1,
                "an unknown outcome must not invite a retry"
            );
        }
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
