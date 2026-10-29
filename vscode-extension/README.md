# Gitpane Explorer

Gitpane Explorer adds a repository overview to VS Code's Explorer. See branches
and changed files across the repositories in your current workspace. Expand a
repository to see staged, unstaged, untracked and conflicted files. Select a
change to open it in the editor.

Gitpane reads changes from Git, including untracked files that VS Code's Git
settings hide. Large change lists are shown one page at a time. When the list
is not fully loaded, the repository's change count is a lower bound. Symlink
diffs show the link target text stored by Git. A repository with more than
200,000 change records, or a snapshot exceeding the 64 MiB cache limit, shows
an error instead of an incomplete list.

The view includes detected repositories inside the open workspace folders,
including multi-root workspaces. Linked worktrees inside the workspace appear
under their repository, with their own changed files and actions. A worktree
added as a workspace folder appears at the top level. Repositories outside the
workspace are not included.

The view updates as workspace folders and detected repositories change. While
visible, it also refreshes local Git status periodically. The `$` indicator
opens a repository's stash list. Stash entries are view-only.
Select a repository or worktree and press `/` or the search button to search
all of its changed paths. Use the arrow keys to move between matches, Enter to
open a file diff, and Escape to clear the search.
The Explorer title menu offers repository sorting; the default keeps the
existing dirty-first, full-path order.

Gitpane relies on VS Code's Git extension to detect repositories. Its Git
auto-detection, scan depth and ignored-repository settings affect which nested
repositories appear. Gitpane opens detected linked worktrees inside the
workspace through VS Code's Git API, which also adds them to Source Control
and can reopen a worktree closed there.

Select the `●` beside a repository to open its commit graph. Search highlights
matches among the loaded commits, with next and previous navigation. The graph
shows up to 200 commits and offers first-parent, branch, author and ref filters,
and branch collapse. Author choices come from commits loaded so far. Select a
commit and then a file to open its diff; the file list is searchable too.
The graph uses Gitpane's `[graph]` settings for branch visibility, label length
and diff statistics. Restart the VS Code extension host after changing them.
The sidebar and graph use Gitpane's configured theme and adapt its colors for
the VS Code background. Use **Gitpane: Choose Theme…** to preview and save a
theme for Gitpane. Restart the extension host after editing theme files directly.

The `⋯` menus provide file actions to stage, unstage, discard or delete an
untracked file. Repository actions include branch review, push, pull, pull with
rebase and worktree creation. Linked worktrees inside the workspace can be
removed from their menu.
An explicitly added linked worktree can also be removed from its repository
menu when it is the only repository in the workspace. If its folder is a
workspace root, the folder is removed from disk and remains listed in the
workspace until you remove it from the workspace configuration.
Gitpane refuses to remove a worktree containing another workspace folder.
Worktree creation suggests Gitpane's configured `[worktree] dir`, or a sibling
of the repository when unset. If that location is outside the workspace or
unavailable, choose an existing parent directory inside the workspace instead.
Restart the VS Code extension host after changing the worktree configuration.

The repository menu also opens GitHub issues and pull requests for a
`github.com` remote. Filter by open, closed or all, inspect descriptions,
comments and available pull request diffs, or open an item on GitHub. Each
list contains up to 50 issues and 50 pull requests. This feature requires the
GitHub CLI (`gh`) on the workspace host, with network access and authentication
for the repository. GitHub Enterprise hosts are not supported.

Select a changed submodule to inspect its commit graph, committed pointer
changes and current file changes. The submodule must be initialized and inside
the workspace. Staged submodule renames, type changes and conflicts open a
pointer comparison; staged deletions also open a pointer comparison. Use
**Inspect Submodule** from a change menu to open an available checkout.
If a deleted submodule leaves a corrupt checkout behind, Gitpane reports the
repository status as unavailable until that checkout is repaired or removed.
Blob and symlink sides in these comparisons must be UTF-8 text under 1 MiB.
Working submodule renames and type changes cannot be compared yet. Working changes
in nested submodules can be inspected, and staged or conflicted nested gitlinks
open pointer comparisons. Historical nested pointer diffs are not
available yet. The parent repository's file actions cannot stage, unstage, discard or
delete a submodule entry. Gitpane's push and pull actions do not recurse into
submodules. On Unix, the repository menu can sync direct submodules, update
submodules recursively, or pull the latest changes in direct submodules. It
reports skipped modules and partial failures. Each repository is checked
against the workspace before Git acts on it. Pull with Submodules first pulls
the parent, then updates the submodules that were already initialized; it
reports partial completion if a later step fails.

The repository menu can open an integrated terminal or find and open tmux or
herdr sessions belonging to the repository on the workspace host. tmux
navigation uses Gitpane's `[goto]` configuration. Session navigation and
submodule actions are not yet supported on Windows. Remote workspace hosts may
not be able to focus a local terminal window.

Use **Gitpane: Fetch Workspace** or the cloud download button to fetch the
current workspace repositories. Each linked worktree is fetched separately so
its own branch and remote configuration apply. Repositories sharing a Git
common directory are fetched in sequence. Gitpane reports failures for each
repository and refreshes the status and visible graph afterward.

Use **Pin Submodule…** from a repository or worktree menu to pin a direct
submodule, including one without changes. The list comes from VS Code and can
be refreshed; you can also enter a registered relative path. To pin a clean
nested submodule, first open its parent repository in VS Code. You can also use
**Pin Submodule** on a submodule change, or choose **Pin Submodule in Gitpane**
while inspecting nested submodules.
Pinned submodules are restored when their root repository is open in the same
workspace and their registered paths and repository identities still match.
Use **Unpin Submodule** from its repository menu to remove the pin. Gitpane
never closes or hides a repository discovered by VS Code when unpinning. The
repository can remain visible after its pin marker is removed.
Use **Hide from Gitpane** in a repository or worktree menu to remove its row
from the overview. **Gitpane: Restore Hidden Repositories** restores it.
This preference belongs to the current workspace and leaves VS Code's Git
repositories and workspace folders unchanged. Newly detected repositories
remain visible. Hiding a pinned submodule does not remove its pin.
If VS Code has not discovered a repository inside an open workspace folder,
use **Gitpane: Add Workspace Repository…** and choose its root folder. Gitpane
restores explicitly added repositories while their paths and identities still
match. **Stop Restoring Repository** or **Gitpane: Manage Workspace
Repositories** removes that preference without closing the repository in VS
Code. If an open request is still running, wait for it to finish before
removing the saved addition. VS Code cannot cancel an open already in progress.
Use **Gitpane: Manage Submodule Pins** to remove a saved pin that can no longer
be restored.
If VS Code is already opening a pinned repository, wait for that open to finish
before removing its pin. VS Code does not provide a way to cancel an open in
progress. If the workspace changes during that call, the repository may still
open, but Gitpane rejects the stale pin and does not close the repository.

The ◉ marker shows a live session for a repository or worktree. Set
`gitpane.sessionBackend` to `tmux` or `herdr` when VS Code does not inherit the
multiplexer environment. The default, `auto`, uses the detected host. Gitpane's
`ui.show_liveness` setting also controls the marker.

Configured Launchers offers Gitpane's `[open]` and custom keybinding commands
in integrated terminals. Command and inline placements are supported; other
placements and interpreter executables report an error rather than changing
their behavior silently.

Discard and delete require confirmation and check the selected file for changes
before running Git. Files larger than 64 MiB, directories and special files
cannot be discarded or deleted through these actions. Another process can still
change a file between the check and the Git operation. On Windows, an ancestor
path can also change during the check; Windows behavior has not yet been
verified locally.

## Package and install

The VSIX bundles Gitpane from this checkout. Build and install it on the machine
where the extension will run. For Apple Silicon macOS:

```sh
node vscode-extension/scripts/package-vsix.js --target darwin-arm64 --out "$HOME/Downloads/gitpane-explorer.vsix"
code --install-extension "$HOME/Downloads/gitpane-explorer.vsix"
```

The extension requires VS Code 1.111 or newer. The packaging script requires
Node.js 22 or newer, npm, Rust and network access to run the pinned VS Code
packaging tool. It does not overwrite an existing output file. Supported
targets are `darwin-arm64`, `darwin-x64`, `linux-arm64`,
`linux-x64` and `win32-x64`. Build on the target OS and architecture. Linux
packages require glibc, and the corresponding Rust target must be installed.

The extension starts its bundled `gitpane serve` backend. Set
`gitpane.backendPath` to an absolute path on the workspace host to use a
different compatible Gitpane build. The extension reports errors from that
executable; it does not switch to another backend.

## Development host

Build Gitpane from this checkout, then start a development host from
`vscode-extension`:

```sh
cargo build --locked --manifest-path ../Cargo.toml
code --new-window --extensionDevelopmentPath="$(pwd)"
```

In the development host, set `gitpane.backendPath` to the absolute path of the
built `target/debug/gitpane` executable. Open a folder or workspace, then expand
Gitpane in the Explorer. On Windows, the executable is `target/debug/gitpane.exe`.
For SSH, WSL or a development container, install a
VSIX built for that workspace host. Virtual and untrusted workspaces are not
supported. Git and VS Code's built-in Git extension must be available on the
workspace host. VS Code's Git settings may enable automatic fetching.

Run the JavaScript tests with `npm test` in this directory. The extension has
no npm dependencies. To check the view in headless Chrome, run
`node test/integration/browser.js` and `node test/integration/graph-browser.js`.
The optional VS Code host test at
`node test/integration/run.js` opens a separate VS Code window.
