use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn exchange(config: &Path, mutation: bool, requests: &[Value]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gitpane"));
    command.args(["--theme", "muted", "serve"]);
    if mutation {
        command.arg("--mutation-once");
    }
    let mut child = command
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
fn serve_honors_cli_theme_and_rejects_persisting_with_active_override() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "theme = 'default'\n").unwrap();
    let initialize =
        json!({"id":1,"method":"initialize","params":{"version":1,"workspace_roots":[]}});
    let read = exchange(
        &config,
        false,
        &[
            initialize.clone(),
            json!({"id":2,"method":"theme","params":{}}),
            json!({"id":3,"method":"theme_preview","params":{"name":"default"}}),
        ],
    );
    assert_eq!(read[1]["result"]["name"], "muted");
    let token = read[2]["result"]["preview_token"].as_str().unwrap();
    let saved = exchange(
        &config,
        true,
        &[
            initialize,
            json!({"id":2,"method":"theme_save","params":{"name":"default","expected_current":"default","preview_token":token}}),
        ],
    );
    assert_eq!(saved[1]["error"]["code"], "theme_error");
    assert!(
        saved[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--theme")
    );
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "theme = 'default'\n"
    );
}
