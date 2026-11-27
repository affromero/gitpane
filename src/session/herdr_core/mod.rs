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

/// Where the calling terminal sits: the labels a new terminal must carry to
/// appear beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// The space to open in. Carrying it is what makes a terminal visible.
    pub(crate) space: String,
    /// The tab to open in, when the calling terminal has one. Placing the new
    /// terminal in the same tab splits it beside gitpane; without it the window
    /// starts a new tab in the space.
    pub(crate) tab: Option<String>,
}

/// Read the calling terminal's own labels.
///
/// Fails rather than guessing: a caller with no terminal, a terminal that has
/// finished, and a terminal that never opted into the window are all reported,
/// because each needs the person to do something different. Only a query that
/// could not run at all leaves the space unknown — that is an error too, never
/// a silent "no space".
pub(crate) fn source() -> Result<Source, String> {
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
    parse_source(&output.stdout, &terminal)
}

fn parse_source(output: &[u8], terminal: &str) -> Result<Source, String> {
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
    let labels = value.get("labels").and_then(Value::as_object);
    let space = labels
        .and_then(|labels| labels.get("herdr.space"))
        .and_then(Value::as_str);
    let space = match space {
        Some(space) if valid_label_value(space) => space.to_string(),
        Some(_) => return Err("this terminal's herdr.space label is not a usable id".into()),
        None => {
            return Err(
                "this terminal is not displayed in the Herdr window, so there is no space to \
                 open a terminal in; run gitpane in a Herdr-managed terminal, or set \
                 [open] placement = \"inline\""
                    .into(),
            );
        }
    };
    // A tab is optional: a terminal can be in a space without being in a tab
    // the window names, and an unusable id simply means "let the window pick".
    let tab = labels
        .and_then(|labels| labels.get("herdr.tab"))
        .and_then(Value::as_str)
        .filter(|tab| valid_label_value(tab))
        .map(str::to_string);
    Ok(Source { space, tab })
}

/// `hc run --bg --outlive-parent --no-notify --cwd <dir> --label …` as separate
/// argv.
///
/// The new terminal keeps its parent link (the window shows it under the
/// terminal it came from) but survives gitpane quitting, and nothing is typed
/// into the parent. The labels are what make it visible at all, and they
/// precede the `--` so they stay options, while the command after `--` stays
/// program data even when it starts with a dash. No command opens the machine's
/// own shell.
///
/// `herdr.space` alone puts the terminal in that space as a new tab.
/// `herdr.tab` as well puts it in the calling tab, which the window shows as a
/// split beside gitpane. The direction is the window's choice: a core has no
/// pane geometry to express right or down.
pub(crate) fn run_argv(dir: &str, command: Option<&str>, source: &Source) -> Vec<String> {
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
    argv.push(format!("herdr.space={}", source.space));
    if let Some(tab) = &source.tab {
        argv.push("--label".to_string());
        argv.push(format!("herdr.tab={tab}"));
    }
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

/// Whether a reported directory starts at a root, in either path convention.
/// A terminal on another machine reports its own platform's paths, so a single
/// host's `is_absolute` is the wrong test: it answers for this host's syntax,
/// not the one the path was written in.
fn is_rooted(cwd: &str) -> bool {
    cwd.starts_with('/') || cwd.starts_with('\\') || cwd.as_bytes().get(1) == Some(&b':')
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
        // `cwd` is the directory the terminal reported, which for a terminal on
        // another machine follows that machine's path syntax. Judging it with
        // this host's `is_absolute` would drop every remote terminal on
        // Windows, and drop Unix ones when gitpane itself runs there. A path
        // that starts at a root is what `live_sessions` matches against, so
        // accept anything rooted in either convention.
        if !is_rooted(cwd) {
            continue;
        }
        panes.push((address.to_string(), PathBuf::from(cwd)));
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

    fn at(space: &str, tab: Option<&str>) -> Source {
        Source {
            space: space.into(),
            tab: tab.map(str::to_string),
        }
    }

    #[test]
    fn run_argv_opens_a_visible_terminal_at_the_directory() {
        // A space alone puts the terminal in that space as a new tab.
        assert_eq!(
            run_argv("/code/app", None, &at("spc1", None)),
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
    }

    #[test]
    fn run_argv_places_the_terminal_beside_gitpane_when_the_tab_is_known() {
        // Space plus tab is what the window reads as "beside this terminal".
        assert_eq!(
            run_argv("/code/app", None, &at("spc1", Some("tab9"))),
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
                "--label",
                "herdr.tab=tab9",
            ]
        );
        // Labels precede `--` so they stay options, and the command after it
        // stays program data even when it starts with a dash.
        assert_eq!(
            run_argv(
                "/code/app",
                Some("git diff -U5 HEAD"),
                &at("spc1", Some("tab9"))
            ),
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
                "--label",
                "herdr.tab=tab9",
                "--",
                "sh",
                "-c",
                "git diff -U5 HEAD",
            ]
        );
    }

    #[test]
    fn source_reads_the_live_terminals_own_labels() {
        let out = terminal(
            "live",
            "\"herdr.space\":\"spc104fd\",\"herdr.tab\":\"tab9\",\"where\":\"repo / tab\"",
        );
        assert_eq!(
            parse_source(&out, "t17").unwrap(),
            at("spc104fd", Some("tab9"))
        );
        // A terminal can be in a space without a usable tab: the window then
        // picks, and the space alone still makes the launch visible.
        assert_eq!(
            parse_source(&terminal("live", "\"herdr.space\":\"spc104fd\""), "t17").unwrap(),
            at("spc104fd", None)
        );
        assert_eq!(
            parse_source(
                &terminal(
                    "live",
                    "\"herdr.space\":\"spc104fd\",\"herdr.tab\":\"tab 9\""
                ),
                "t17"
            )
            .unwrap(),
            at("spc104fd", None)
        );
    }

    #[test]
    fn source_refuses_a_terminal_that_already_finished() {
        // The environment outlives the terminal, so a valid id and a leftover
        // label prove nothing: only a live terminal can place a new tab.
        let out = terminal("finished", "\"herdr.space\":\"spc104fd\"");
        let error = parse_source(&out, "t17").unwrap_err();
        assert!(error.contains("finished"), "{error}");
        assert!(error.contains("restart gitpane"), "{error}");
    }

    #[test]
    fn source_separates_a_missing_label_from_a_broken_reply() {
        let hidden = terminal("live", "\"where\":\"repo / tab\"");
        let error = parse_source(&hidden, "t17").unwrap_err();
        assert!(error.contains("no space to open a terminal in"), "{error}");
        assert!(error.contains("inline"), "{error}");

        // A reply about another terminal, a missing state and bad JSON are
        // backend failures, never "this terminal has no space".
        let other = terminal("live", "\"herdr.space\":\"spc1\"");
        assert!(
            parse_source(&other, "t99")
                .unwrap_err()
                .contains("different terminal")
        );
        let stateless = br#"{"address":"t17","labels":{"herdr.space":"spc1"}}"#;
        assert!(
            parse_source(stateless, "t17")
                .unwrap_err()
                .contains("missing state")
        );
        assert!(
            parse_source(b"not json", "t17")
                .unwrap_err()
                .contains("invalid hc")
        );
        // A label the window could not accept is not usable either.
        let padded = br#"{"address":"t17","state":"live","labels":{"herdr.space":"spc 1"}}"#;
        assert!(
            parse_source(padded, "t17")
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
    fn parse_terminals_keeps_paths_from_either_platform() {
        // A terminal on another machine reports its own path syntax. Judging it
        // with this host's `is_absolute` would drop every remote terminal when
        // gitpane runs on Windows, and Unix ones when it runs there.
        let out = ls(concat!(
            r#"{"address":"t1","state":"live","cwd":"/code/app"},"#,
            r#"{"address":"t2","state":"live","cwd":"C:\\code\\app"},"#,
            r#"{"address":"t3","state":"live","cwd":"\\\\server\\share"},"#,
            r#"{"address":"t4","state":"live","cwd":"relative/path"},"#,
            r#"{"address":"t5","state":"live","cwd":""}"#
        ));
        assert_eq!(
            parse_terminals(&out).unwrap(),
            vec![
                ("t1".to_string(), PathBuf::from("/code/app")),
                ("t2".to_string(), PathBuf::from("C:\\code\\app")),
                ("t3".to_string(), PathBuf::from("\\\\server\\share")),
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
