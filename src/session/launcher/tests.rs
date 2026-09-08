use super::*;

fn argv(parts: &[&str]) -> LaunchPlan {
    LaunchPlan::Spawn(parts.iter().map(|s| s.to_string()).collect())
}

#[test]
fn goto_placement_infers_tab_or_window() {
    assert_eq!(
        goto_placement("wezterm cli spawn -- tmux attach -t {session}"),
        Some("new tab")
    );
    assert_eq!(
        goto_placement("kitten @ launch --type=tab tmux attach -t {session}"),
        Some("new tab")
    );
    assert_eq!(
        goto_placement("open -na Ghostty --args -e tmux attach -t {session}"),
        Some("new window")
    );
    assert_eq!(goto_placement("tmux switch-client -t {session}"), None);
}

#[test]
fn goto_argv_substitutes_session() {
    assert_eq!(
        build_goto_argv("tmux switch-client -t {session}", "fairtrail"),
        vec!["tmux", "switch-client", "-t", "fairtrail"]
    );
    assert_eq!(
        build_goto_argv("wezterm cli spawn -- tmux attach -t {session}", "ft-rec"),
        vec![
            "wezterm", "cli", "spawn", "--", "tmux", "attach", "-t", "ft-rec"
        ]
    );
}

#[test]
fn parse_keywords_and_tmux_and_invalid() {
    assert_eq!(parse_placement("command"), Ok(Placement::Command));
    assert_eq!(parse_placement("inline"), Ok(Placement::Inline));
    assert_eq!(parse_placement("ask"), Ok(Placement::Ask));
    assert_eq!(
        parse_placement("split-window -h -t agents"),
        Ok(Placement::Tmux(
            ["split-window", "-h", "-t", "agents"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        ))
    );
    assert!(parse_placement("kill-server").is_err());
    assert!(parse_placement("-t other").is_err());
    // A `;` would chain extra tmux commands — rejected even after a valid head.
    assert!(parse_placement("split-window -h ; kill-server").is_err());
    assert!(parse_placement("new-window;kill-server").is_err());
}

#[test]
fn command_mode_runs_detached_argv() {
    // open's default: the command is the launcher, run as argv (no shell).
    assert_eq!(
        plan(
            Some("cursor {path}"),
            "command",
            "/w t/app",
            None,
            Multiplexer::None
        ),
        argv(&["cursor", "/w t/app"])
    );
}

#[test]
fn file_launches_keep_the_parent_directory_separate_from_the_file_target() {
    let dir = "/code/my repo";
    let target = "/code/my repo/source.rs";
    let command = "viewer '/code/my repo/source.rs'";
    let cases = [
        ("command", Multiplexer::None, argv(&["viewer", target])),
        (
            "inline",
            Multiplexer::None,
            LaunchPlan::Inline(command.into()),
        ),
        (
            "split-window",
            Multiplexer::Tmux,
            argv(&["tmux", "split-window", "-c", dir, "sh", "-c", command]),
        ),
        (
            "new-window",
            Multiplexer::Herdr,
            LaunchPlan::Herdr {
                create: ["herdr", "tab", "create", "--cwd", dir, "--no-focus"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                command: Some(command.into()),
            },
        ),
    ];
    for (placement, mux, expected) in cases {
        assert_eq!(
            plan_with_target(Some("viewer {path}"), placement, dir, target, None, mux),
            expected,
            "file launch with {placement} placement"
        );
    }
}

#[test]
fn review_command_resolves_base_without_splitting_paths() {
    assert_eq!(
        plan(
            Some("git difftool --dir-diff {base}...HEAD -- {path}"),
            "command",
            "/w t/repo",
            Some("origin/main"),
            Multiplexer::None
        ),
        argv(&[
            "git",
            "difftool",
            "--dir-diff",
            "origin/main...HEAD",
            "--",
            "/w t/repo"
        ])
    );
}

#[test]
fn argv_substitution_preserves_placeholder_text_inside_values() {
    assert_eq!(
        plan(
            Some("viewer {base} {path}"),
            "command",
            "/code/{base}",
            Some("{path}"),
            Multiplexer::None
        ),
        argv(&["viewer", "{path}", "/code/{base}"])
    );
}

#[cfg(unix)]
#[test]
fn shell_substitution_passes_paths_and_refs_as_literal_values() {
    let path = "/code/{base}/a'b$(printf injected)";
    let base = "feature'{path}$(printf injected)";
    let LaunchPlan::Inline(command) = plan(
        Some("printf '%s\\n' {path} {base}"),
        "inline",
        path,
        Some(base),
        Multiplexer::None,
    ) else {
        panic!("inline placement must produce a shell command");
    };
    let output = std::process::Command::new("sh")
        .args(["-c", &command])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{path}\n{base}\n")
    );
}

#[test]
fn command_mode_empty_opens_tmux_pane_in_tmux() {
    assert_eq!(
        plan(None, "command", "/app", None, Multiplexer::Tmux),
        argv(&["tmux", "split-window", "-c", "/app"])
    );
}

#[test]
fn command_mode_empty_without_tmux_errors() {
    assert!(matches!(
        plan(None, "command", "/app", None, Multiplexer::None),
        LaunchPlan::Error(_)
    ));
}

#[test]
fn tmux_placement_wraps_in_sh_c() {
    assert_eq!(
        plan(
            Some("git diff {base}...HEAD"),
            "new-window",
            "/app",
            Some("origin/main"),
            Multiplexer::Tmux
        ),
        argv(&[
            "tmux",
            "new-window",
            "-c",
            "/app",
            "sh",
            "-c",
            "git diff 'origin/main'...HEAD"
        ])
    );
}

#[test]
fn tmux_placement_passes_flags_through() {
    assert_eq!(
        plan(
            Some("lazygit"),
            "split-window -h -t agents",
            "/app",
            None,
            Multiplexer::Tmux
        ),
        argv(&[
            "tmux",
            "split-window",
            "-h",
            "-t",
            "agents",
            "-c",
            "/app",
            "sh",
            "-c",
            "lazygit"
        ])
    );
}

#[test]
fn tmux_placement_without_tmux_falls_back_to_inline() {
    assert_eq!(
        plan(
            Some("git diff {base}...HEAD | delta"),
            "new-window",
            "/app",
            Some("main"),
            Multiplexer::None
        ),
        LaunchPlan::Inline("git diff 'main'...HEAD | delta".to_string())
    );
}

#[test]
fn base_with_metacharacters_is_quoted() {
    assert_eq!(
        plan(
            Some("git diff {base}...HEAD"),
            "inline",
            "/app",
            Some("a;rm -rf b"),
            Multiplexer::None
        ),
        LaunchPlan::Inline("git diff 'a;rm -rf b'...HEAD".to_string())
    );
}

#[test]
fn ask_is_a_picker_in_tmux_and_inline_without() {
    assert_eq!(
        plan(Some("x"), "ask", "/app", None, Multiplexer::Tmux),
        LaunchPlan::Ask
    );
    assert_eq!(
        plan(Some("x"), "ask", "/app", None, Multiplexer::None),
        LaunchPlan::Inline("x".to_string())
    );
}

#[test]
fn command_mode_expands_embedded_path_token() {
    // `{path}` inside a token expands too (one argv element, space-safe).
    assert_eq!(
        plan(
            Some("wezterm cli spawn --cwd={path}"),
            "command",
            "/w t/x",
            None,
            Multiplexer::None
        ),
        argv(&["wezterm", "cli", "spawn", "--cwd=/w t/x"])
    );
}

#[test]
fn command_mode_blank_command_opens_tmux_pane() {
    // A whitespace-only command counts as empty.
    assert_eq!(
        plan(Some("   "), "command", "/repo", None, Multiplexer::Tmux),
        argv(&["tmux", "split-window", "-c", "/repo"])
    );
}

#[test]
fn shell_mode_quotes_path_token() {
    // In shell modes, {path} is shell-quoted (it reaches `sh -c`).
    assert_eq!(
        plan(
            Some("cd {path} && git diff"),
            "inline",
            "/w t/x",
            None,
            Multiplexer::None
        ),
        LaunchPlan::Inline("cd '/w t/x' && git diff".to_string())
    );
}

#[test]
fn invalid_placement_is_an_error_plan() {
    assert!(matches!(
        plan(Some("x"), "frobnicate", "/app", None, Multiplexer::Tmux),
        LaunchPlan::Error(_)
    ));
}

#[test]
fn parse_tmux_windows_skips_malformed_lines() {
    let out = "@0\tmain:0 editor\n@1\t\nno-tab-here\n@2\twork:2 logs\n";
    assert_eq!(
        parse_tmux_windows(out),
        vec![
            ("main:0 editor".to_string(), "@0".to_string()),
            ("@1".to_string(), "@1".to_string()), // empty label -> target as label
            ("work:2 logs".to_string(), "@2".to_string()),
        ]
    );
}

#[test]
fn placement_choices_use_space_free_window_id_target() {
    // Even with a spaced label, the placement `-t` target is the window id,
    // so the whitespace-split placement string stays valid.
    let windows = vec![("my session:0 editor".to_string(), "@7".to_string())];
    assert_eq!(
        placement_choices(&windows),
        vec![
            ("New window".to_string(), "new-window".to_string()),
            (
                "Right of my session:0 editor".to_string(),
                "split-window -h -t @7".to_string()
            ),
            (
                "Below my session:0 editor".to_string(),
                "split-window -v -t @7".to_string()
            ),
        ]
    );
}

fn herdr_argv(parts: &[&str]) -> LaunchPlan {
    LaunchPlan::Herdr {
        create: parts.iter().map(|s| s.to_string()).collect(),
        command: None,
    }
}

#[test]
fn command_mode_empty_opens_herdr_pane_in_herdr() {
    assert_eq!(
        plan(None, "command", "/app", None, Multiplexer::Herdr),
        herdr_argv(&[
            "herdr",
            "pane",
            "split",
            "--current",
            "--direction",
            "right",
            "--cwd",
            "/app",
            "--no-focus",
            "--right-click",
            "pane",
        ])
    );
}

#[test]
fn herdr_split_placement_honors_h_v_and_pane_target() {
    // `-h` -> right, `-v` -> down, `-t <pane-id>` -> `--pane`.
    assert_eq!(
        plan(
            Some("lazygit"),
            "split-window -h",
            "/app",
            None,
            Multiplexer::Herdr
        ),
        LaunchPlan::Herdr {
            create: vec![
                "herdr",
                "pane",
                "split",
                "--current",
                "--direction",
                "right",
                "--cwd",
                "/app",
                "--no-focus",
                "--right-click",
                "pane",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            command: Some("lazygit".to_string()),
        }
    );
    let down = plan(
        Some("x"),
        "split-window -v",
        "/app",
        None,
        Multiplexer::Herdr,
    );
    assert!(matches!(
        down,
        LaunchPlan::Herdr {
            create: ref c,
            ..
        } if c.contains(&"down".to_string())
    ));
    let targeted = plan(
        Some("x"),
        "split-window -h -t w1:p3",
        "/app",
        None,
        Multiplexer::Herdr,
    );
    assert!(matches!(
        targeted,
        LaunchPlan::Herdr {
            create: ref c,
            ..
        } if c.contains(&"--pane".to_string()) && c.contains(&"w1:p3".to_string())
    ));
}

#[test]
fn herdr_new_window_creates_a_tab() {
    // review's default `new-window` placement -> `herdr tab create`; the
    // command runs in the tab's root pane via `herdr pane run`.
    assert_eq!(
        plan(
            Some("git diff {base}...HEAD"),
            "new-window",
            "/app",
            Some("origin/main"),
            Multiplexer::Herdr,
        ),
        LaunchPlan::Herdr {
            create: vec!["herdr", "tab", "create", "--cwd", "/app", "--no-focus",]
                .into_iter()
                .map(String::from)
                .collect(),
            command: Some("git diff 'origin/main'...HEAD".to_string()),
        }
    );
}

#[test]
fn herdr_rejects_unknown_and_tmux_only_flags() {
    // Unknown flags and `new-window` flags must not silently mis-launch.
    assert!(matches!(
        plan(
            Some("x"),
            "split-window -l 20",
            "/app",
            None,
            Multiplexer::Herdr
        ),
        LaunchPlan::Error(_)
    ));
    assert!(matches!(
        plan(
            Some("x"),
            "new-window -t work",
            "/app",
            None,
            Multiplexer::Herdr
        ),
        LaunchPlan::Error(_)
    ));
    assert!(matches!(
        plan(
            Some("x"),
            "split-window -t",
            "/app",
            None,
            Multiplexer::Herdr
        ),
        LaunchPlan::Error(_)
    ));
}

#[test]
fn ask_is_a_picker_under_herdr() {
    assert_eq!(
        plan(Some("x"), "ask", "/app", None, Multiplexer::Herdr),
        LaunchPlan::Ask
    );
}

#[test]
fn herdr_placement_choices_offer_tab_and_splits() {
    assert_eq!(
        herdr_placement_choices(),
        vec![
            ("New tab".to_string(), "new-window".to_string()),
            (
                "Right of current pane".to_string(),
                "split-window -h".to_string(),
            ),
            (
                "Below current pane".to_string(),
                "split-window -v".to_string(),
            ),
        ]
    );
}

#[test]
fn parse_herdr_pane_id_reads_split_and_tab_responses() {
    let split = "{\"id\":\"cli:pane:split\",\"result\":{\"pane\":{\"pane_id\":\"w1:p3\"}}}";
    assert_eq!(parse_herdr_pane_id(split), Some("w1:p3".to_string()));
    let tab = "{\"result\":{\"tab\":{\"tab_id\":\"w1:t2\"},\"root_pane\":{\"pane_id\":\"w1:p7\"}}}";
    assert_eq!(parse_herdr_pane_id(tab), Some("w1:p7".to_string()));
    assert_eq!(parse_herdr_pane_id("not json"), None);
}
