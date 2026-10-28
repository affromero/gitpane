# Gitpane Explorer

Gitpane Explorer adds a repository overview to VS Code's Explorer. See branches
and changed files across the repositories in your current workspace. Expand a
repository to see staged, unstaged, untracked and conflicted files. Select a
change to open it in the editor.

The view uses VS Code's built-in Git extension. It includes detected repositories
inside the open workspace folders, including multi-root workspaces. A linked
worktree appears when it is inside a workspace folder and VS Code detects it.
Repositories outside the workspace are not included.

The view updates as workspace folders and detected repositories change. While
visible, it also refreshes local Git status periodically. Submodule changes
are listed but cannot yet be opened as file diffs.

## Package and install

From the repository root, build a VSIX and install it in VS Code:

```sh
node vscode-extension/scripts/package-vsix.js --out "$HOME/Downloads/gitpane-explorer.vsix"
code --install-extension "$HOME/Downloads/gitpane-explorer.vsix"
```

The packaging script requires Node.js 22 or newer, npm, and network access to
run the pinned VS Code packaging tool. It will not overwrite an existing output
file. The VSIX contains no platform-specific binary and does not require the
Gitpane CLI. VS Code's built-in Git extension and Git must be available in the
workspace environment.

## Development host

From `vscode-extension`, start a development host with:

```sh
code --new-window --extensionDevelopmentPath="$(pwd)"
```

Open a folder or workspace in the development host, then expand Gitpane in the
Explorer. For SSH, WSL or a development container, the extension and Git run on
the workspace host. Virtual workspaces and untrusted workspaces are not
supported. The extension does not fetch remotes itself; VS Code's Git settings
may independently enable automatic fetching.

Run the JavaScript tests with `npm test` in this directory. The extension has
no npm dependencies. To check the view in headless Chrome, run
`node test/integration/browser.js`. The optional VS Code host test at
`node test/integration/run.js` opens a separate VS Code window.
