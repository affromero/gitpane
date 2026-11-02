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
        if mode == "hidden" {
            assert_eq!(Multiplexer::detect(), Multiplexer::None);
            return;
        }
        assert_eq!(Multiplexer::detect(), Multiplexer::HerdrApp);
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
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join("layout.json"), layout_value().to_string()).unwrap();
    for (name, script) in [
        (
            "herdr-app",
            "#!/bin/sh\ncase \"$1\" in\nls) IFS= read -r data < \"$GITPANE_DESKTOP_TEST_ROOT/layout.json\"; printf '%s' \"$data\";;\ndo) printf '%s\\n' \"$@\" >> \"$GITPANE_DESKTOP_TEST_ROOT/calls\"; printf '%s' '{\"pane\":\"p-created\",\"terminal\":\"t9\"}';;\n*) exit 3;;\nesac\n",
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
    let path = std::env::join_paths(std::iter::once(root.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    for (mode, address) in [("visible", "t1"), ("hidden", "t99")] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "session::herdr_app::tests::official_backend_uses_displayed_membership_and_explicit_targets", "--nocapture"])
            .env(CHILD, mode).env("HC_TERMINAL_ID", address)
            .env("GITPANE_DESKTOP_TEST_ROOT", root).env("PATH", &path)
            .env_remove("HERDR_ENV").env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_TAB_ID").env_remove("HERDR_WORKSPACE_ID")
            .env_remove("TMUX").env_remove("TMUX_PANE")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
