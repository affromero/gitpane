# AGENTS.md

Guidance for AI coding agents working on **gitpane**, a multi-repo Git workspace
dashboard TUI (Rust, [ratatui](https://github.com/ratatui/ratatui)). For the
human-facing overview, see [README.md](README.md).

## Setup

- Rust edition 2024, MSRV **1.88.0**. Install a matching toolchain (rustup recommended).
- No services or network access are needed to build, run, or test.
- Optional tooling for the full local suite: `cargo install cargo-audit cargo-llvm-cov`,
  plus [`just`](https://github.com/casey/just) for the task recipes below.

## Build and run

```bash
just run                 # or: cargo run
cargo build --release
```

## Checks (run before every commit)

```bash
just ci                  # fmt + lint + docs + test, the full gate
# or individually:
just fmt                 # cargo fmt --all
just lint                # cargo clippy --all-targets --all-features -- -D warnings
just test                # cargo test --all-targets --all-features
just docs                # rustdoc with -D warnings
```

CI runs the same checks and treats every warning as an error, so `just ci` must
pass before you push. Tests are inline `#[cfg(test)]` modules in the binary
target, so a plain `cargo test --lib` finds nothing. Use `just test` or
`cargo test --bin gitpane`.

## Pre-commit hooks (required)

Install them before contributing:

```bash
pre-commit install       # installs the pre-commit and pre-push hooks
```

The pre-commit hook runs file hygiene, TOML/YAML checks, `cargo fmt`, `cargo
clippy`, and the two project rules below. The pre-push hook runs tests, audit,
docs, and coverage.

## Hard constraints

- **Every source file stays under 1000 lines.** When a file approaches the limit,
  split it: turn `foo.rs` into a `foo/` directory module and re-export the public
  items from `foo/mod.rs` so external paths such as `crate::foo::Bar` do not
  change. Do not add exclude lists to dodge the limit; split the file.
- **Every directory under `src/` holds at most 10 files.** Group related modules
  into a subfolder rather than letting a folder sprawl. A subfolder's files count
  toward the subfolder, not its parent.
- **No warnings.** clippy runs with `-D warnings`. Fix the cause; do not paper over
  it with `#[allow(...)]` unless there is a documented reason.
- Refactors that relocate code must be behavior preserving: keep the move
  mechanical, re-export to preserve public paths, and keep the test count identical.

## Architecture

Message passing: terminal and filesystem events become `Event`s, the app
dispatches `Action`s, and components update and render. See the diagram and module
map in [README.md#architecture](README.md#architecture). Key areas:

- `src/app/` main loop and action dispatch (`handle_action` / `handle_action_rest`),
  launching, input handling, and rendering.
- `src/components/` ratatui widgets, each implementing the `Component` trait.
- `src/git/` libgit2 (`git2`) status, graph, and commit diffs, run inside
  `tokio::task::spawn_blocking` to keep the UI responsive.
- `src/session/` tmux and terminal session integration (launcher and liveness).
- `src/config/` TOML config load/save and the terminal auto-detect table.

## Conventions

- Conventional commit subjects: `feat:`, `fix:`, `docs:`, `refactor:`, `chore:`,
  `test:`, `build:`. Keep each commit focused and leave the tree building and green.
- Test behavior and outcomes, not internals, and put tests next to the code they
  cover.
- Prefer a pure, testable core: a function returns a plan or value, and a thin
  caller performs the I/O (see `src/session/launcher.rs`).
- Validate or quote any user input that reaches a shell. argv launches avoid the
  shell entirely; `sh -c` paths quote every substituted value.

## Pull requests

- Open an issue first (bug report or feature request) and reference it with
  `Closes #<n>` in the PR body. Skip the issue only for trivial fixes (typos,
  doc tweaks).
- The web UI pre-fills `.github/PULL_REQUEST_TEMPLATE.md`, but `gh pr create`
  does not — structure the PR body yourself with the template's sections:
  `Closes #<n>`, `## In simple terms` (one or two sentences a user
  would understand), `## Problem`, `## Fix`, `## Test`.

## Releasing

Releases are tag driven. Pushing a `vX.Y.Z` tag triggers
`.github/workflows/release.yml`, which builds the platform binaries, creates the
GitHub Release from the matching `CHANGELOG.md` section, and publishes to
crates.io. Do not publish manually.
