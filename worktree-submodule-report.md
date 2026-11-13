# Worktree submodule reproduction

The VS Code extension integration fixture creates a repository, a linked
worktree outside the workspace, and an initialized `external-child` submodule
inside that worktree. It creates a commit in that submodule and refreshes
Gitpane. At the baseline commit, Gitpane showed the worktree, but its snapshot
had no row for `external-child`.

Run the reproduction from this checkout:

```sh
cargo build
GITPANE_TEST_BINARY="$PWD/target/debug/gitpane" \
  node vscode-extension/test/integration/extension-real.js
```

On macOS, against commit `08061f74621b7b17c3b1f9dc92c719dd9c0947d3`, the
test reached the external worktree and failed with:

```text
AssertionError [ERR_ASSERTION]: an initialized submodule must appear under its worktree
```

At that commit, the extension built outside-workspace worktree rows from a
synthetic repository whose submodule list was empty. Automatic submodule
discovery only considered workspace repositories. The regression now checks
the child row, its changes, its graph, and rejection after its registration
changes. The command passes with the changes in this worktree.

The TUI also used the main checkout's path and status when opening a submodule
from a worktree changes panel. The regression
`selecting_a_worktree_submodule_uses_its_checkout_and_status` checks that the
worktree checkout owns the selected submodule and that its delete shortcut does
not target the parent repository.
The TUI also lists each checkout's clean and changed submodules under that
checkout, and keeps a selected submodule selected when its status changes.
