# VS Code reload tests

The reload tests open staged and working diffs in a dedicated VS Code profile and shared-data directory,
reload the window, and read both sides from the restored tabs. They require a
built Gitpane backend and a VS Code executable on `PATH`, or an absolute
`VSCODE_EXECUTABLE` path.

```sh
GITPANE_TEST_BINARY="$(pwd)/target/debug/gitpane" node vscode-extension/test/integration/reload.js
```

The Remote SSH variant also needs Docker, network access to download the VS
Code server and Remote SSH extension, and a Linux backend matching the Docker
host architecture. It creates a private SSH key, pins the server host key,
binds SSH to loopback, and removes its container and image after the run.

```sh
GITPANE_LINUX_TEST_BINARY="$(pwd)/target/release/gitpane" node vscode-extension/test/integration/reload-remote.js
```

On a headless Linux host, prefix either command with `xvfb-run -a`. The remote
test verifies that VS Code reports an `ssh-remote` extension host. Failed runs
retain their private test directory and report its path for diagnosis.
