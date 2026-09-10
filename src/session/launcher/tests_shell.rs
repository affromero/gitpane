use super::*;

fn run(command: &str, directory: &std::path::Path) -> String {
    let output = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn shell_launches_preserve_values_in_each_quote_context_without_executing_them() {
    let directory = tempfile::tempdir().unwrap();
    let target = "source café ' \" $(touch INJECTED) `touch INJECTED` {base}\nfile.rs";
    let base = "branch'\";touch INJECTED;${HOME} {path}";
    for template in [
        "printf '%s\\n' {path} {base}",
        "printf '%s\\n' \"{path}\" \"{base}\"",
        "printf '%s\\n' '{path}' '{base}'",
        "# ignored '\" {path} ${HOME}\nprintf '%s\\n' {path} {base}",
        "printf '%s\\n' pre{path}post 'pre{base}post'",
    ] {
        for (placement, mux) in [
            ("inline", Multiplexer::None),
            ("new-window", Multiplexer::None),
            ("split-window", Multiplexer::Tmux),
            ("new-window", Multiplexer::Herdr),
        ] {
            let dir = directory.path().to_str().unwrap();
            let launch = plan_with_target(Some(template), placement, dir, target, Some(base), mux);
            let command = match launch {
                LaunchPlan::Inline(command) => command,
                LaunchPlan::Spawn(argv) => {
                    assert_eq!(&argv[..4], ["tmux", "split-window", "-c", dir]);
                    assert_eq!(&argv[4..6], ["sh", "-c"]);
                    argv[6].clone()
                }
                LaunchPlan::Herdr { create, command } => {
                    assert_eq!(
                        create,
                        ["herdr", "tab", "create", "--cwd", dir, "--no-focus"]
                    );
                    command.expect("configured command")
                }
                other => panic!("unexpected launch: {other:?}"),
            };
            let expected = if template.contains("pre{path}") {
                format!("pre{target}post\npre{base}post\n")
            } else {
                format!("{target}\n{base}\n")
            };
            assert_eq!(
                run(&command, directory.path()),
                expected,
                "{template} / {placement}"
            );
            assert!(!directory.path().join("INJECTED").exists());
        }
    }
}

#[test]
fn shell_launches_keep_working_directory_and_pipes() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("space ' directory");
    std::fs::create_dir(&target).unwrap();
    let LaunchPlan::Inline(command) = plan(
        Some(
            "cd \"{path}\" && printf '%s\\n' \"$PWD\" | while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
        ),
        "inline",
        target.to_str().unwrap(),
        None,
        Multiplexer::None,
    ) else {
        panic!("expected inline command");
    };
    assert_eq!(
        run(&command, directory.path()).trim(),
        target.to_str().unwrap()
    );
}

#[test]
fn shell_launches_preserve_existing_environment_and_positional_parameters() {
    let LaunchPlan::Inline(command) = plan(
        Some("printf '%s\\n' \"$GITPANE_TEST_VALUE\" \"$1\" \"{path}\""),
        "inline",
        "/tmp/repo",
        None,
        Multiplexer::None,
    ) else {
        panic!("expected inline command");
    };
    let output = std::process::Command::new("sh")
        .args(["-c", &command])
        .env("GITPANE_TEST_VALUE", "preserved")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "preserved\n\n/tmp/repo\n"
    );
}

#[test]
fn shell_launches_reject_ambiguous_or_incomplete_placeholder_templates() {
    for template in [
        "printf '%s' \\{path}",
        "printf '%s' \"{path}",
        "printf '%s' '{path}",
        "printf '%s' {path} \\",
        "printf '%s' $(printf '%s' {path})",
        "printf '%s' `printf '%s' {path}`",
        "printf '%s' ${HOME:-{path}}",
        "printf '%s' $'{path}'",
        "printf '%s' $\"{path}\"",
        "cat <<EOF\n{path}\nEOF",
    ] {
        for placement in ["inline", "split-window", "ask"] {
            assert!(
                matches!(
                    plan(
                        Some(template),
                        placement,
                        "/tmp/repo",
                        None,
                        Multiplexer::Tmux
                    ),
                    LaunchPlan::Error(_)
                ),
                "accepted {template:?} with {placement}"
            );
        }
    }
    assert!(matches!(
        plan(
            Some("printf '%s' {base}"),
            "inline",
            "/tmp/repo",
            None,
            Multiplexer::None
        ),
        LaunchPlan::Error(_)
    ));
}
