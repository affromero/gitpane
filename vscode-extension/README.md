# Gitpane Explorer

Gitpane Explorer adds a repository overview to VS Code's Explorer. Expand a
repository to see staged, unstaged, untracked and conflicted files. Select a
change to open it in the editor. Changes refresh as VS Code's Git extension
updates its repositories. VS Code's Git auto-refresh settings affect live
updates; use the Gitpane refresh button to refresh local status manually.

## Run the prototype

1. Build the Gitpane binary from this checkout with `cargo build`.
2. From `vscode-extension`, start a development host with
   `code --new-window --extensionDevelopmentPath="$(pwd)"`. This requires the
   VS Code `code` command on your PATH.
3. In the development host, set `gitpane.executable` to the absolute path of
   the built binary, such as `target/debug/gitpane` in the parent checkout.
   Open the Explorer and expand Gitpane. Use the refresh button after adding
   or removing repositories.

Gitpane reads its configured roots, pins and exclusions from the host where the
extension runs. With SSH, WSL or a development container, install the binary and
configure it on that host. Virtual workspaces and untrusted workspaces are not
supported. VS Code's built-in Git extension and Git executable must be enabled.
The view includes discovered repositories and their linked worktrees, plus
repositories associated with the open workspace. If discovery fails, the view
shows the previous discovery result with an error until a refresh succeeds.
Submodule changes are listed but cannot yet be opened as file diffs.

The prototype registers discovered repositories with VS Code's Git extension,
so they also appear in Source Control and may increase its background work.
Refreshing can reopen a repository that was closed in Source Control.
It does not fetch from remotes itself. VS Code's Git settings may independently
enable automatic fetching. Errors are shown in the Gitpane view and output
channel. Linked worktrees are included when VS Code exposes them through its
Git API; refresh after a worktree is created.

Run the JavaScript tests with `npm test` in this directory. The extension has
no npm dependencies.
