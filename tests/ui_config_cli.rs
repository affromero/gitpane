use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn exchange(config: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gitpane"))
        .arg("serve")
        .env("GITPANE_CONFIG", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn worktree_visibility_uses_the_effective_config_and_requires_initialization() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let requests = [
        json!({"id":1,"method":"ui_config","params":{}}),
        json!({"id":2,"method":"initialize","params":{"version":1,"workspace_roots":[]}}),
        json!({"id":3,"method":"ui_config","params":{}}),
        json!({"id":4,"method":"ui_config","params":{"unexpected":true}}),
    ];
    for (text, expected, keys, review) in [
        ("", true, json!([]), false),
        ("[ui]\nexpand_worktrees = false\n", false, json!([]), false),
        (
            "[review]\ncommand = 'viewer'\n[[keybindings]]\nkey = 'j'\ncommand = 'editor'\n[[keybindings]]\nkey = 'Ctrl+j'\ncommand = 'editor'\n[[keybindings]]\nkey = '🦀'\ncommand = 'editor'\n",
            true,
            json!(["j", "🦀"]),
            true,
        ),
    ] {
        std::fs::write(&config, text).unwrap();
        let replies = exchange(&config, &requests);
        assert_eq!(replies[0]["error"]["code"], "not_initialized");
        assert_eq!(replies[1]["result"]["version"], 1);
        assert_eq!(
            replies[2]["result"],
            json!({"expand_worktrees": expected,
            "launcher_keys": keys, "review_configured": review})
        );
        assert_eq!(replies[3]["error"]["code"], "invalid_request");
    }
}
