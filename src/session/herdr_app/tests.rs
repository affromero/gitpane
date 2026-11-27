use super::*;
use crate::session::env::Multiplexer;
#[cfg(unix)]
use crate::session::launcher::plan_with_target;
use crate::session::launcher::{LaunchPlan, plan};

fn layout_value() -> Value {
    json!({"spaces": [
        {"id": "sp-source", "name": "source space", "machine": null, "active": false,
         "tabs": [{"id": "tb-source", "name": "source tab", "active": false,
            "terminals": [{"terminal": "t1", "pane": "p-source"},
                          {"terminal": "t2", "pane": "p-neighbor"},
                          {"terminal": "t3", "pane": "p-finished"}]}]},
        {"id": "sp-focused", "name": "focused space", "machine": null, "active": true,
         "tabs": [{"id": "tb-focused", "name": "focused tab", "active": true,
            "terminals": [{"terminal": "t4", "pane": "p-focused"}]}]},
        {"id": "remote/sp-source", "machine": "remote",
         "tabs": [{"id": "remote/tb-source",
            "terminals": [{"terminal": "remote/t1", "pane": "remote/p-source"}]}]}
    ]})
}

fn fixture() -> (Layout, Context) {
    let layout = parse_layout(&serde_json::to_vec(&layout_value()).unwrap()).unwrap();
    let source = layout.context("t1").unwrap();
    (layout, source)
}

#[test]
fn desktop_context_retries_after_failure_and_does_not_cache_hidden_workers() {
    let cache = OnceLock::new();
    assert_eq!(
        pin_context(&cache, "t1", || Err("desktop offline".into())).unwrap_err(),
        "desktop offline"
    );
    assert_eq!(
        pin_context(&cache, "t1", || Ok(LayoutProbe::Unavailable(
            "no desktop".into()
        )))
        .unwrap(),
        None
    );
    assert_eq!(
        pin_context(&cache, "t99", || Ok(LayoutProbe::Available(fixture().0))).unwrap(),
        None
    );
    let context = pin_context(&cache, "t1", || Ok(LayoutProbe::Available(fixture().0)))
        .unwrap()
        .unwrap();
    assert_eq!(context.pane, "p-source");
    for terminal in ["t1", "t2", ""] {
        assert_eq!(
            pin_context(&cache, terminal, || panic!("context should be pinned")).unwrap(),
            Some(context)
        );
    }
}

#[test]
fn liveness_preserves_other_terminals_when_a_pane_closes_during_scan() {
    let (layout, _) = fixture();
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let mut updated = layout_value();
    updated["spaces"][0]["tabs"][0]["terminals"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    let sessions = pane_sessions_with(
        layout,
        |address| {
            if address == "t2" {
                return Err("terminal disappeared".into());
            }
            Ok(json!({"address": address, "state": "live", "cwd": cwd}))
        },
        || parse_layout(&serde_json::to_vec(&updated).unwrap()),
    )
    .unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|(address, _)| address.as_str())
            .collect::<Vec<_>>(),
        ["t1", "t3", "t4"]
    );
}

#[test]
fn liveness_surfaces_terminal_errors_when_the_pane_is_still_displayed() {
    let (layout, _) = fixture();
    let error = pane_sessions_with(layout, |_| Err("hc unavailable".into()), || Ok(fixture().0))
        .unwrap_err();
    assert_eq!(error, "hc unavailable");
}

#[test]
fn terminal_membership_maps_to_actual_containing_pane_space_and_tab() {
    let (layout, source) = fixture();
    assert_eq!(
        source,
        Context {
            terminal: "t1".into(),
            pane: "p-source".into(),
            tab: "tb-source".into(),
            space: "sp-source".into(),
        }
    );
    assert!(
        layout.context("t99").is_err(),
        "hidden HC workers are not app panes"
    );
    assert!(
        layout.context("p-source").is_err(),
        "pane IDs are not terminal addresses"
    );
    assert!(
        layout.context("remote/t1").is_err(),
        "remote paths must not become local repos"
    );
}

#[test]
fn malformed_or_ambiguous_layouts_fail_instead_of_guessing_a_focused_target() {
    for value in [
        json!({}),
        json!({"spaces": [{"id": "sp", "tabs": null}]}),
        json!({"error": "offline"}),
    ] {
        assert!(parse_layout(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for field in ["pane", "terminal"] {
        let mut value = layout_value();
        value["spaces"][0]["tabs"][0]["terminals"][1][field] =
            value["spaces"][0]["tabs"][0]["terminals"][0][field].clone();
        assert!(parse_layout(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = layout_value();
    value["spaces"][0]["tabs"][0]["terminals"][0]["pane"] = json!("bad\npane");
    assert!(parse_layout(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn creation_argv_pins_source_and_honors_explicit_pane_tab_and_space_targets() {
    let (layout, source) = fixture();
    let argv = vec!["sh".into(), "-c".into(), "printf 'literal'".into()];
    for (placement, command, target, direction) in [
        ("split-window", "pane.split", "p-source", Some("right")),
        ("split-window -v", "pane.split", "p-source", Some("down")),
        (
            "split-window -h -t p-neighbor",
            "pane.split",
            "p-neighbor",
            Some("right"),
        ),
        (
            "split-window -t p-focused -v",
            "pane.split",
            "p-focused",
            Some("down"),
        ),
        ("new-window", "tab.new", "sp-source", None),
        ("new-window -t sp-focused", "tab.new", "sp-focused", None),
        ("new-window -t tb-focused", "tab.new", "sp-focused", None),
        ("new-window -t p-focused", "tab.new", "sp-focused", None),
    ] {
        let result = create_argv(
            &layout,
            &source,
            placement,
            &argv,
            Path::new("/selected dir"),
        )
        .unwrap();
        assert_eq!(&result[..3], ["herdr-app", "do", command]);
        assert_eq!(result.len(), 4);
        let payload: Value = serde_json::from_str(&result[3]).unwrap();
        let expected = if let Some(direction) = direction {
            json!({"direction": direction, "pane": target, "argv": argv})
        } else {
            json!({"space": target, "folder": "/selected dir", "argv": argv})
        };
        assert_eq!(payload, expected, "{placement}");
    }
    for invalid in [
        "split-window -t t2",
        "split-window -t missing",
        "split-window -t remote/p-source",
        "split-window -t",
        "split-window -t p-source -t p-neighbor",
        "split-window -p 50",
        "new-window -h",
        "kill-server",
        "new-window -t missing",
    ] {
        assert!(
            create_argv(&layout, &source, invalid, &argv, Path::new("/repo")).is_err(),
            "{invalid}"
        );
    }
    let mut changed = source.clone();
    changed.pane = "p-neighbor".into();
    assert!(create_argv(&layout, &changed, "new-window", &argv, Path::new("/repo")).is_err());
}

#[test]
fn ask_choices_have_explicit_targets_in_the_containing_space() {
    let (layout, source) = fixture();
    let choices = choices(&layout, &source).unwrap();
    assert_eq!(choices.len(), 7);
    assert_eq!(choices[0].1, "new-window -t sp-source");
    for (_, value) in &choices[1..] {
        assert!(value.contains(" -t p-"));
        assert!(!value.contains("focused") && !value.contains("remote"));
        validate_placement(value).unwrap();
    }
    assert!(
        choices
            .iter()
            .any(|(_, value)| value == "split-window -h -t p-source")
    );
    assert!(
        choices
            .iter()
            .any(|(_, value)| value == "split-window -v -t p-neighbor")
    );
}

#[test]
fn desktop_plans_cover_default_open_review_ask_and_invalid_flags() {
    let LaunchPlan::HerdrApp { placement, argv } = plan(
        None,
        "command",
        "/selected dir",
        None,
        Multiplexer::HerdrApp,
    ) else {
        panic!("default open must launch a shell in the desktop");
    };
    assert_eq!(placement, "split-window -h");
    assert_eq!(&argv[..2], ["sh", "-c"]);
    assert_eq!(&argv[3..], ["gitpane", "/selected dir"]);
    assert!(argv[2].contains("exec \"${SHELL:-/bin/sh}\" -l"));
    assert_eq!(
        plan(Some("viewer"), "ask", "/repo", None, Multiplexer::HerdrApp),
        LaunchPlan::Ask
    );
    assert_eq!(
        plan(
            Some("viewer {path}"),
            "command",
            "/repo path",
            None,
            Multiplexer::HerdrApp
        ),
        LaunchPlan::Spawn(vec!["viewer".into(), "/repo path".into()])
    );
    for placement in [
        "split-window -h",
        "split-window -v -t p-source",
        "new-window -t sp-source",
    ] {
        assert!(matches!(
            plan(
                Some("review {base}"),
                placement,
                "/repo",
                Some("main"),
                Multiplexer::HerdrApp
            ),
            LaunchPlan::HerdrApp { .. }
        ));
    }
    for placement in ["split-window -p 30", "new-window -d", "split-window -t"] {
        assert!(matches!(
            plan(
                Some("viewer"),
                placement,
                "/repo",
                None,
                Multiplexer::HerdrApp
            ),
            LaunchPlan::Error(_)
        ));
    }
}

#[test]
fn liveness_uses_foreground_cwd_skips_finished_and_rejects_wrong_identity() {
    let directory = tempfile::tempdir().unwrap();
    let launch = directory.path().join("launch");
    let foreground = directory.path().join("actual").join("nested");
    let value = json!({"address":"t1","state":"live","cwd":launch,"foreground":{"cwd":foreground}});
    assert_eq!(
        terminal_cwd(value.clone(), "t1").unwrap().as_deref(),
        Some(foreground.as_path())
    );
    assert!(terminal_cwd(value, "t2").is_err());
    assert_eq!(
        terminal_cwd(
            json!({"address":"t1","state":"live","cwd":launch,"foreground":null}),
            "t1"
        )
        .unwrap()
        .as_deref(),
        Some(launch.as_path())
    );
    assert_eq!(
        terminal_cwd(
            json!({"address":"t1","state":"finished","cwd":launch}),
            "t1"
        )
        .unwrap(),
        None
    );
    assert!(
        terminal_cwd(
            json!({"address":"t1","state":"live","cwd":"relative"}),
            "t1"
        )
        .is_err()
    );
    assert!(terminal_cwd(json!({"address":"t1","state":"unknown","cwd":launch}), "t1").is_err());
    assert!(terminal_cwd(json!({"address":"t1","state":"live"}), "t1").is_err());
}

#[test]
fn launch_json_errors_and_malformed_success_responses_remain_visible() {
    validate_launch_response(br#"{"pane":"p-new","terminal":"t9"}"#).unwrap();
    for value in [
        b"null".as_slice(),
        b"[]",
        b"not-json",
        br#"{"error":"target closed"}"#,
    ] {
        assert!(validate_launch_response(value).is_err());
    }
    let error =
        validate_launch_response(br#"{"error":{"kind":"denied","message":"target closed"}}"#)
            .unwrap_err();
    assert!(error.contains("target closed") && error.contains("denied"));
}

#[cfg(unix)]
#[test]
fn desktop_probe_only_falls_back_for_missing_cli_or_unreachable_desktop() {
    use std::io::{Error, ErrorKind};
    use std::os::unix::process::ExitStatusExt;

    for kind in [
        ErrorKind::NotFound,
        ErrorKind::PermissionDenied,
        ErrorKind::TimedOut,
        ErrorKind::Other,
    ] {
        match classify_layout(Err(Error::new(kind, "probe failed"))) {
            Ok(LayoutProbe::Unavailable(message)) => {
                assert_eq!(kind, ErrorKind::NotFound);
                assert!(message.contains("probe failed"));
            }
            Err(message) => {
                assert_ne!(kind, ErrorKind::NotFound);
                assert!(message.contains("probe failed"));
            }
            _ => panic!("unexpected desktop layout"),
        }
    }
    let missing = format!(
        "herdr-app: no Herdr window at /test/herdr-app.sock: {}",
        Error::from_raw_os_error(libc::ENOENT)
    );
    let refused = format!(
        "herdr-app: no Herdr window at /test/herdr-app.sock: {}",
        Error::from_raw_os_error(libc::ECONNREFUSED)
    );
    for (status, stdout, stderr, absent) in [
        (2, "", missing.as_str(), true),
        (2, "", refused.as_str(), true),
        (1, "", missing.as_str(), false),
        (2, "{}", missing.as_str(), false),
        (
            2,
            "",
            "herdr-app: no Herdr window at /test/herdr-app.sock: Permission denied (os error 13)",
            false,
        ),
        (
            2,
            "",
            "herdr-app: no Herdr window at /test/herdr-app.sock: path must be shorter than SUN_LEN",
            false,
        ),
        (
            2,
            "",
            "herdr-app: no Herdr window at /test/herdr-app.sock: unknown (os error 2)",
            false,
        ),
        (2, "", "herdr-app: layout request failed", false),
        (
            2,
            r#"{"error":{"reason":"layout request failed"}}"#,
            "",
            false,
        ),
    ] {
        let result = classify_layout(Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(status << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }));
        match result {
            Ok(LayoutProbe::Unavailable(message)) => {
                assert!(absent, "{message}");
                assert!(message.contains(stderr));
            }
            Err(message) => {
                assert!(!absent, "{message}");
                assert!(!message.is_empty());
            }
            _ => panic!("unexpected desktop layout"),
        }
    }
}

#[cfg(unix)]
#[test]
fn rejected_desktop_commands_keep_structured_stdout_error_reasons() {
    use std::os::unix::process::ExitStatusExt;
    for (stdout, stderr, expected) in [
        (
            br#"{"error":"target closed"}"#.as_slice(),
            b"".as_slice(),
            "\"target closed\"",
        ),
        (
            br#"{"error":{"reason":"permission denied"}}"#,
            b"",
            r#"{"reason":"permission denied"}"#,
        ),
        (b"not-json", b"socket unavailable\n", "socket unavailable"),
    ] {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        assert_eq!(error_detail(&output), expected);
    }
}

#[cfg(unix)]
#[test]
fn desktop_review_argv_executes_at_literal_cwd_with_safe_path_and_base_substitution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let cwd = root.join("repo '$(touch unsafe)\n{base}");
    std::fs::create_dir(&cwd).unwrap();
    let target = cwd.join("file '{base}$(touch unsafe).rs");
    let base = "topic'$(touch unsafe){path}";
    let LaunchPlan::HerdrApp { argv, .. } = plan_with_target(
        Some("printf '%s\\n' \"$PWD\" \"{path}\" '{base}'"),
        "split-window -v",
        cwd.to_str().unwrap(),
        target.to_str().unwrap(),
        Some(base),
        Multiplexer::HerdrApp,
    ) else {
        panic!("expected desktop review argv")
    };
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n{}\n{base}\n", cwd.display(), target.display())
    );
    assert!(!root.join("unsafe").exists());
    assert!(!cwd.join("unsafe").exists());
}

#[cfg(unix)]
#[test]
fn official_backend_uses_displayed_membership_and_explicit_targets() {
    use std::os::unix::fs::PermissionsExt;
    const CHILD: &str = "GITPANE_DESKTOP_TEST_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        if mode != "visible" {
            let detected = Multiplexer::detect();
            match mode.as_str() {
                // No displayed desktop: an HC terminal is a herdr-core terminal,
                // which is exactly how a hidden worker or remote terminal runs.
                "hidden" | "missing-cli" | "no-desktop" | "desktop-stopped" => {
                    assert_eq!(detected.unwrap(), Multiplexer::HerdrCore);
                }
                // These carry `$TMUX`, but the stub PATH has no reachable tmux
                // server to confirm ownership, so the nested case falls back to
                // the calling core terminal. A reachable tmux keeps Tmux — see
                // `resolve_host` in `session::env`.
                "missing-cli-tmux" | "no-desktop-tmux" | "hidden-tmux" => {
                    assert_eq!(detected.unwrap(), Multiplexer::HerdrCore);
                }
                "no-desktop-legacy" => {
                    assert_eq!(detected.unwrap(), Multiplexer::Herdr);
                }
                "bad-json" => assert!(detected.unwrap_err().contains("invalid herdr-app layout")),
                "permission" => assert!(detected.unwrap_err().contains("Permission denied")),
                "cli-failure" => assert!(detected.unwrap_err().contains("layout request failed")),
                _ => panic!("unknown desktop test mode: {mode}"),
            }
            assert!(current_context().is_err());
            assert!(launcher_choices("new-window").is_err());
            return;
        }
        assert_eq!(Multiplexer::detect().unwrap(), Multiplexer::HerdrApp);
        let source = current_context().unwrap();
        assert_eq!(source.pane, "p-source");
        let directory = PathBuf::from(std::env::var_os("GITPANE_DESKTOP_TEST_ROOT").unwrap());
        launch(
            "split-window -v",
            &["printf".into(), "literal $(data)".into()],
            &directory,
            source,
        )
        .unwrap();
        let calls = std::fs::read_to_string(directory.join("calls")).unwrap();
        let lines = calls.lines().collect::<Vec<_>>();
        assert_eq!(&lines[..2], ["do", "pane.split"]);
        let payload: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(
            payload,
            json!({"direction":"down","pane":"p-source","argv":["printf","literal $(data)"]})
        );
        let sessions = pane_sessions().unwrap();
        assert_eq!(
            sessions,
            vec![
                ("t1".into(), PathBuf::from("/foreground")),
                ("t2".into(), PathBuf::from("/launch")),
                ("t4".into(), PathBuf::from("/other"))
            ]
        );
        focus("t2").unwrap();
        assert!(focus("t99").is_err());
        let calls = std::fs::read_to_string(directory.join("calls")).unwrap();
        assert!(calls.ends_with("do\nagent.focus\n{\"terminal\":\"t2\"}\n"));
        let probes = std::fs::read_to_string(directory.join("probes")).unwrap();
        assert_eq!(probes.lines().collect::<Vec<_>>(), ["t1", "t2", "t3", "t4"]);
        std::fs::write(directory.join("offline"), "").unwrap();
        assert_eq!(detected_context().unwrap(), Some(source));
        assert_eq!(Multiplexer::detect().unwrap(), Multiplexer::HerdrApp);
        let error = prepare_launch("new-window", &["sh".into()], &directory, source).unwrap_err();
        assert!(error.contains("no Herdr window at"));
        std::fs::remove_file(directory.join("offline")).unwrap();
        let mut moved = layout_value();
        moved["spaces"][0]["tabs"][0]["terminals"][0]["pane"] = json!("p-moved");
        std::fs::write(directory.join("layout.json"), moved.to_string()).unwrap();
        assert_eq!(detected_context().unwrap(), Some(source));
        let error = prepare_launch("new-window", &["sh".into()], &directory, source).unwrap_err();
        assert!(error.contains("source pane changed"));
        assert_eq!(
            std::fs::read_to_string(directory.join("calls")).unwrap(),
            calls
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let empty_path = root.join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    std::fs::write(root.join("layout.json"), layout_value().to_string()).unwrap();
    for (name, script) in [
        (
            "herdr-app",
            r#"#!/bin/sh
case "$1" in
ls)
    if [ -f "$GITPANE_DESKTOP_TEST_ROOT/offline" ]; then
        printf '%s\n' 'herdr-app: no Herdr window at /test/herdr-app.sock: No such file or directory (os error 2)' >&2
        exit 2
    fi
    case "$GITPANE_DESKTOP_TEST_CHILD" in
        no-desktop*)
            printf '%s\n' 'herdr-app: no Herdr window at /test/herdr-app.sock: No such file or directory (os error 2)' >&2
            exit 2;;
        permission)
            printf '%s\n' 'herdr-app: no Herdr window at /test/herdr-app.sock: Permission denied (os error 13)' >&2
            exit 2;;
        desktop-stopped)
            printf 'herdr-app: no Herdr window at /test/herdr-app.sock: %s\n' "$GITPANE_DESKTOP_TEST_REFUSED" >&2
            exit 2;;
        cli-failure)
            printf '%s\n' 'herdr-app: layout request failed' >&2
            exit 2;;
        bad-json) printf '%s' 'not-json'; exit 0;;
    esac
    IFS= read -r data < "$GITPANE_DESKTOP_TEST_ROOT/layout.json"
    printf '%s' "$data";;
do)
    printf '%s\n' "$@" >> "$GITPANE_DESKTOP_TEST_ROOT/calls"
    printf '%s' '{"pane":"p-created","terminal":"t9"}';;
*) exit 3;;
esac
"#,
        ),
        (
            "hc",
            "#!/bin/sh\n[ \"$1\" = get ] || exit 3\nprintf '%s\\n' \"$2\" >> \"$GITPANE_DESKTOP_TEST_ROOT/probes\"\ncase \"$2\" in\nt1) printf '%s' '{\"address\":\"t1\",\"state\":\"live\",\"cwd\":\"/launch\",\"foreground\":{\"cwd\":\"/foreground\"}}';;\nt2) printf '%s' '{\"address\":\"t2\",\"state\":\"live\",\"cwd\":\"/launch\"}';;\nt3) printf '%s' '{\"address\":\"t3\",\"state\":\"finished\",\"cwd\":\"/gone\"}';;\nt4) printf '%s' '{\"address\":\"t4\",\"state\":\"live\",\"cwd\":\"/other\"}';;\n*) exit 3;;\nesac\n",
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (mode, address) in [
        ("no-desktop", "t1"),
        ("desktop-stopped", "t1"),
        ("missing-cli", "t1"),
        ("missing-cli-tmux", "t1"),
        ("no-desktop-tmux", "t1"),
        ("no-desktop-legacy", "t1"),
        ("permission", "t1"),
        ("cli-failure", "t1"),
        ("bad-json", "t1"),
        ("hidden", "t99"),
        ("hidden-tmux", "t99"),
        ("visible", "t1"),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "session::herdr_app::tests::official_backend_uses_displayed_membership_and_explicit_targets", "--nocapture"])
            .env(CHILD, mode).env("HC_TERMINAL_ID", address)
            .env("GITPANE_DESKTOP_TEST_ROOT", root)
            .env("GITPANE_DESKTOP_TEST_REFUSED", std::io::Error::from_raw_os_error(libc::ECONNREFUSED).to_string())
            .env("PATH", if mode.starts_with("missing-cli") { &empty_path } else { root })
            .env_remove("HERDR_ENV").env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_TAB_ID").env_remove("HERDR_WORKSPACE_ID")
            .env_remove("TMUX").env_remove("TMUX_PANE");
        if mode.ends_with("-tmux") {
            command.env("TMUX", "/test/tmux,1,0").env("TMUX_PANE", "%1");
        } else if mode.ends_with("-legacy") {
            command.env("HERDR_ENV", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
