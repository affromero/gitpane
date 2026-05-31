<p align="center">
  <h1 align="center">gitpane</h1>
  <p align="center">
    <strong>Multi-repo Git workspace dashboard for the terminal</strong>
  </p>
  <p align="center">
    <a href="https://github.com/affromero/gitpane/actions/workflows/ci.yml"><img src="https://github.com/affromero/gitpane/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://crates.io/crates/gitpane"><img src="https://img.shields.io/crates/v/gitpane.svg" alt="crates.io"></a>
    <a href="https://github.com/affromero/gitpane/releases/latest"><img src="https://img.shields.io/github/v/tag/affromero/gitpane?label=release" alt="GitHub Release"></a>
    <a href="https://crates.io/crates/gitpane"><img src="https://img.shields.io/crates/d/gitpane?label=downloads" alt="Downloads"></a>
    <a href="https://github.com/affromero/gitpane/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
    <img src="https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-informational" alt="Platform">
    <img src="https://img.shields.io/github/languages/top/affromero/gitpane" alt="Language">
    <a href="https://socket.dev"><img src="https://img.shields.io/badge/Socket-protected-blueviolet?logo=socket.dev" alt="Socket"></a>
  </p>
</p>

---

Monitor **all your repos at a glance** — branch, dirty state, ahead/behind, active worktrees, changed files, and commit history — without leaving the terminal.

<p align="center">
  <img src="assets/demo.gif" alt="gitpane demo" width="800">
</p>

## Install

```bash
cargo install gitpane
```

That's it. No cloning, no building from source. Runs on **Linux, macOS, and Windows**.

> **Don't have Rust?** Download a pre-built binary from [GitHub Releases](https://github.com/affromero/gitpane/releases/latest) — single static binary, zero dependencies.
>
> ```bash
> # macOS (Apple Silicon)
> curl -LO https://github.com/affromero/gitpane/releases/latest/download/gitpane-aarch64-apple-darwin.tar.gz
> tar xzf gitpane-aarch64-apple-darwin.tar.gz && sudo mv gitpane /usr/local/bin/
>
> # macOS (Intel)
> curl -LO https://github.com/affromero/gitpane/releases/latest/download/gitpane-x86_64-apple-darwin.tar.gz
> tar xzf gitpane-x86_64-apple-darwin.tar.gz && sudo mv gitpane /usr/local/bin/
>
> # Linux (x86_64, statically linked)
> curl -LO https://github.com/affromero/gitpane/releases/latest/download/gitpane-x86_64-unknown-linux-musl.tar.gz
> tar xzf gitpane-x86_64-unknown-linux-musl.tar.gz && sudo mv gitpane /usr/local/bin/
>
> # Linux (ARM64)
> curl -LO https://github.com/affromero/gitpane/releases/latest/download/gitpane-aarch64-unknown-linux-gnu.tar.gz
> tar xzf gitpane-aarch64-unknown-linux-gnu.tar.gz && sudo mv gitpane /usr/local/bin/
> ```

Then run:

```bash
gitpane                     # Scans ~/Code by default
gitpane --root ~/projects   # Scan a specific directory
gitpane diagnostic          # Print config, watcher, and workspace diagnostics
```

## Update

```bash
cargo install gitpane       # Same command — overwrites the old binary
```

If you installed from a [GitHub Release](https://github.com/affromero/gitpane/releases/latest), re-download the latest binary for your platform using the same commands from the install section above.

## Why gitpane?

If you work across multiple repositories — microservices, monorepos with submodules, a mix of projects — you know the pain of `cd`-ing into each one to check status. Existing TUI tools focus on **one repo at a time**:

| Tool | Multi-repo | Auto-refresh | Worktrees | Mouse | Commit graph | Split diffs | Push/Pull |
|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| **gitpane** | **Yes** | **Yes** | **Yes** | **Yes** | **Yes** | **Yes** | **Yes** |
| [lazygit](https://github.com/jesseduffield/lazygit) | No | No | No | Yes | Yes | Yes | Yes |
| [gitui](https://github.com/extrawurst/gitui) | No | No | No | Yes | Yes | Yes | Yes |
| [tig](https://github.com/jonas/tig) | No | No | No | No | Yes | No | No |
| [git-delta](https://github.com/dandavison/delta) | No | No | No | No | No | Yes (pager) | No |
| [grv](https://github.com/rgburke/grv) | No | No | No | Yes | Yes | No | No |
| [git-summary](https://github.com/MircoT/git-summary) | Yes (list only) | No | No | No | No | No | No |
| [mgitstatus](https://github.com/fboender/multi-git-status) | Yes (list only) | No | No | No | No | No | No |
| [gita](https://github.com/nosarthur/gita) | Yes (CLI only) | No | No | No | No | No | Yes |

**lazygit** and **gitui** are excellent for deep single-repo work — staging hunks, interactive rebase, conflict resolution. gitpane is the **workspace-level dashboard** — see everything across all repos, drill into anything, never leave the terminal. They complement each other.

## Screenshots

### Three-panel overview
Repos on the left show branch, dirty state (`*`), ahead/behind arrows (`↑↓`), worktree count (`⎇`), dirty submodules (`◈`), unpushed submodule pointer (`⇡`), stash count (`$`), and file count. Changes in the middle. Commit graph on the right.

<img src="assets/screenshot-main.png" alt="Three-panel overview" width="800">

### Split diff view
Click a changed file (or press Enter) to see its diff side-by-side. File list stays navigable on the left.

<img src="assets/screenshot-diff.png" alt="Split diff view" width="800">

### Commit detail drill-down
Click a commit in the graph to see its files. Click a file to see the commit diff. Layered Esc dismissal: diff → files → graph.

<img src="assets/screenshot-commit.png" alt="Commit detail drill-down" width="800">

## Features

- **Multi-repo overview** — Scans `~/Code` (configurable) for git repos; shows branch, dirty indicator (`*`), ahead/behind arrows (`↑↓`), worktree count (`⎇`), dirty submodule (`◈`), unpushed submodule pointer (`⇡`), stash count (`$`), and change count
- **Worktree awareness** — Shows the number of linked git worktrees per repo (`⎇2`). In the agentic AI era, tools like Claude Code create worktrees for parallel development — gitpane lets you see at a glance which repos have active parallel work
- **Filesystem awareness** — Watches repo roots and Git metadata for commits, checkouts, and new repos; local polling catches nested worktree file changes without overwhelming Linux inotify
- **Commit graph** — Lane-based graph with colored box-drawing characters, up to 200 commits
- **Split diff views** — Click a file to see its diff side-by-side; click a commit to see its files and per-file diffs
- **Full mouse support** — Click to select, right-click for context menu, scroll wheel everywhere
- **Push / Pull / Rebase** — Right-click context menu with ahead/behind-aware git operations (explicit `origin <branch>` for reliability)
- **Add & remove repos** — Press `a` to add any repo with tab-completing path input; `d` to remove; `R` to rescan
- **Sort repos** — Cycle between alphabetical and dirty-first with `s`
- **Copy to clipboard** — Press `y` to copy selected item from any panel (OSC 52)
- **Configurable** — TOML config for root dirs, scan depth, pinned repos, exclusions, frame rate
- **Responsive layout** — Three horizontal panels on wide terminals, vertical stack on narrow ones
- **Cross-platform** — Linux, macOS, Windows

## Keybindings

### Global

| Key | Action |
|-----|--------|
| `?` | Toggle keybindings help overlay |
| `Tab` / `Shift+Tab` | Cycle focus between panels |
| `r` | Refresh all repo statuses |
| `R` | Rescan directories for repos (clears exclusions) |
| `g` | Reload git graph for selected repo |
| `a` | Add a repo (opens path input with tab completion) |
| `d` | Remove selected repo (with confirmation) |
| `s` | Cycle sort order (Alphabetical / Dirty-first) |
| `w` | Toggle worktree subtree for the selected repo |
| `S` | Toggle stash subtree for the selected repo |
| `t` | Open the theme picker (live preview, Enter to persist) |
| `y` | Copy selected item to clipboard |
| `q` | Quit (or close diff if one is open) |
| `Esc` | Navigate back through panels, then quit |

### Repos panel

| Key | Action |
|-----|--------|
| `j` / `↓` | Next repo |
| `k` / `↑` | Previous repo |

### Changes panel

| Key | Action |
|-----|--------|
| `j` / `↓` | Next file |
| `k` / `↑` | Previous file |
| `Enter` | Open split diff view |
| `Esc` / `h` / `←` | Close diff view |

### Graph panel

| Key | Action |
|-----|--------|
| `j` / `↓` | Next commit / file |
| `k` / `↑` | Previous commit / file |
| `h` / `l` | Scroll graph left / right |
| `Enter` | Open commit files / file diff |
| `Esc` | Close diff → close files → back |
| `/` | Search commits (message, author, short ID) |
| `n` / `N` | Next / previous search match |
| `f` | Toggle first-parent mode |
| `c` | Collapse / expand branch |
| `H` | Expand all collapsed branches |

### Mouse

| Action | Effect |
|--------|--------|
| Left click | Select item, switch panel focus |
| Click selected row | Open diff / commit detail |
| Right click (repo list) | Context menu (push, pull, copy path) |
| Scroll wheel | Navigate lists or scroll diffs |

### Path input (`a`)

| Key | Action |
|-----|--------|
| `Tab` | Autocomplete directory path (cycles matches) |
| `Enter` | Add the repo |
| `Esc` | Cancel |
| `Ctrl+A` / `Home` | Cursor to start |
| `Ctrl+E` / `End` | Cursor to end |
| `Ctrl+U` | Clear line before cursor |

## Configuration

gitpane resolves its config file in this order (first existing file wins):

1. `$GITPANE_CONFIG` (if set, treated as the full path; this overrides everything below and is also the save target)
2. `$XDG_CONFIG_HOME/gitpane/config.toml` (if `$XDG_CONFIG_HOME` is set and absolute)
3. `~/.config/gitpane/config.toml` (the XDG default, on every platform)
4. The platform-native location:

| Platform | Path |
|----------|------|
| Linux    | `~/.config/gitpane/config.toml` (same as 3) |
| macOS    | `~/Library/Application Support/gitpane/config.toml` |
| Windows  | `%APPDATA%\gitpane\config\config.toml` |

If no file is found at any candidate path, gitpane uses the built-in defaults (`root_dirs = ["~/Code"]`, `scan_depth = 2`). When saving after loading a file, gitpane writes back to the loaded path. When saving from defaults, it writes to `$GITPANE_CONFIG`, `$XDG_CONFIG_HOME/gitpane/config.toml`, `~/.config/gitpane/config.toml`, or the platform-native location, in that order.

gitpane logs the resolved path at startup (`tracing` info level on stderr).

```toml
# Directories to scan for git repositories
root_dirs = ["~/Code", "~/work"]

# Maximum directory depth for repo discovery
scan_depth = 2

# Always show these repos at the top
pinned_repos = ["~/Code/important-project"]

# Skip repos matching these directory names
excluded_repos = ["node_modules", ".cargo", "target"]

[watch]
debounce_ms = 500             # Filesystem change debounce (ms)
refresh_cooldown_ms = 5000    # Min ms between watcher-triggered status refreshes per repo
watch_worktree_dirs = false   # Opt in to nested worktree watches; polling still catches changes
poll_local_secs = 5           # Local status poll interval (catches missed watcher events)
poll_fetch_secs = 30          # Remote fetch poll interval (updates ahead/behind from origin)
discovery_cooldown_secs = 5   # Min seconds between auto-rescans on root-dir changes (new clones)

[ui]
frame_rate = 10              # Terminal refresh rate (fps)
check_for_updates = true     # Check for new versions on startup
update_position = "top-right" # Update notification position ("top-right" or "top-left")

[graph]
branches = "all"         # Branch filter: "all", "local", "remote", or "none"
label_max_len = 24       # Max length for branch/tag labels
show_stats = true        # Show +N/-M diff stats per commit
```

See [`examples/config.toml`](examples/config.toml) for a fully annotated example.

### Theming

gitpane ships two built-in themes:

- `default`, the original palette (used when `theme` is unset).
- `muted`, softer 256-color indices for dark terminals where the default `Light*` colors feel too bright.

```toml
# In config.toml
theme = "muted"
```

To define a custom theme, drop a TOML file at `<config_dir>/gitpane/themes/<name>.toml` and set `theme = "<name>"`. Any field you don't list falls back to the corresponding `default` slot, so a custom theme can be as small as one override:

```toml
# ~/.config/gitpane/themes/mine.toml
[repo_list]
stash = "Magenta"

[graph]
tag_label = "143"        # 256-color index
lane_palette = ["Red", "#5fafd7", "Cyan", "67", "Magenta", "Yellow"]
```

Color values accept ratatui's standard names (`"Yellow"`, `"LightMagenta"`, ...), 8-bit indices as bare integers (`"67"`), or 24-bit hex (`"#5fafd7"`). If `$GITPANE_CONFIG` points to a non-XDG location, the `themes/` directory next to that file is searched first.

**Switching themes:**

- **From inside the app**: press `t` to open the picker. Up/Down (or `j`/`k`) cycles through themes with live preview, `Enter` saves the choice to `config.toml`, `Esc` cancels and restores.
- **From the shell**: `gitpane --theme muted` overrides the active theme for one run without modifying `config.toml`.
- **List available themes**: `gitpane themes` prints every built-in and custom theme, with a marker on the currently-resolved one.

## Troubleshooting

For a copyable snapshot of the active config, scan roots, watcher settings, repo count, and CPU-pressure warnings, run:

```sh
gitpane diagnostic
gitpane --root ~/projects diagnostic
```

### gitpane shows no repositories

Run through these in order:

**1. Check that gitpane is reading your config file.**

gitpane prints the resolved config path at startup. Run gitpane with stderr captured:

```sh
RUST_LOG=gitpane=info gitpane 2>/tmp/gitpane.log
cat /tmp/gitpane.log
```

You should see `loaded config path=...` or `no config file found, using defaults`. If gitpane is not loading the file you expected, check the candidate paths in [Configuration](#configuration). On macOS, `~/.config/gitpane/config.toml` works as well as the native `~/Library/Application Support/gitpane/config.toml`.

To force a specific file:

```sh
GITPANE_CONFIG=/path/to/config.toml gitpane
```

You can also bypass the config entirely to confirm repo discovery:

```sh
gitpane --root "$HOME/src"
```

If `--root` finds your repos but the config does not, the file path or config contents are the likely issue.

**2. Check that `scan_depth` is large enough.**

`scan_depth` is the maximum directory depth from each entry in `root_dirs` at which gitpane will look for a `.git` directory. For a layout like `~/src/github.com/<owner>/<repo>/.git`, the `.git` lives at depth 4, so you need `scan_depth = 4` (or higher). Counting from the root:

```
~/src                                       depth 0
~/src/github.com                            depth 1
~/src/github.com/<owner>                    depth 2
~/src/github.com/<owner>/<repo>             depth 3
~/src/github.com/<owner>/<repo>/.git        depth 4
```

**3. Check for symlinks in your tree.**

The scanner does not follow symlinks. If any directory along the path to a repo is a symlink (for example `~/src/github.com` pointing at `/mnt/code/github.com`), repos under it will be skipped.

```sh
find    "$HOME/src" -maxdepth 4 -name .git -type d -print
echo '... with -L (follows symlinks):'
find -L "$HOME/src" -maxdepth 4 -name .git -type d -print
```

If the second command finds repos and the first doesn't, that's the cause. Workarounds: point `root_dirs` at the symlink target directly, or list specific repos in `pinned_repos`.

**4. Check whether `.git` is a directory or a file.**

Linked git worktrees and some submodule layouts store `.git` as a *file* containing a `gitdir:` pointer, not a directory. The scanner only matches `.git` directories. To check one repo:

```sh
test -d "$HOME/src/github.com/affromero/gitpane/.git" && echo dir \
 || test -f "$HOME/src/github.com/affromero/gitpane/.git" && echo file
```

If it prints `file`, add the repo via `pinned_repos` instead, or open it explicitly with `gitpane --root /path/to/parent`.

**5. Verbose logging.**

```sh
RUST_LOG=gitpane=debug gitpane 2>/tmp/gitpane.log
```

Then inspect `/tmp/gitpane.log` for any errors during config load or repo scanning.

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                     tokio runtime                        │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────────┐   │
│  │ Event    │→ │ Action   │→ │ Components            │   │
│  │ Loop     │  │ Dispatch │  │  RepoList             │   │
│  │ (tui.rs) │  │ (app.rs) │  │  FileList (split diff)│   │
│  └──────────┘  └──────────┘  │  GitGraph (drill-down)│   │
│       ↑                      │  ContextMenu          │   │
│  ┌──────────┐                │  PathInput             │   │
│  │ notify   │                │  StatusBar             │   │
│  │ watcher  │                └──────────────────────┘   │
│  └──────────┘                                           │
│       ↑              ┌───────────────────────┐           │
│  filesystem          │ git2 (spawn_blocking) │           │
│  changes             │  status · graph       │           │
│                      │  commit_files · fetch │           │
│                      └───────────────────────┘           │
└──────────────────────────────────────────────────────────┘
```

- **[ratatui](https://github.com/ratatui/ratatui)** + **[crossterm](https://github.com/crossterm-rs/crossterm)** — TUI rendering with full mouse support
- **[git2](https://github.com/rust-lang/git2-rs)** (libgit2) — Branch, status, ahead/behind, graph, commit diffs
- **[notify](https://github.com/notify-rs/notify)** — Filesystem watching with configurable debounce
- **[tokio](https://github.com/tokio-rs/tokio)** — Async runtime; git queries run in `spawn_blocking` to keep the UI responsive

Message-passing architecture: terminal events → actions → component updates → render. Each component implements a `Component` trait with `draw`, `handle_key_event`, `handle_mouse_event`, and `update`.

## Development

```bash
just run           # Build and run
just test          # Run test suite (90 tests)
just fmt           # Format code
just lint          # Run clippy
just ci            # fmt + lint + test (mirrors CI pipeline)
```

### Project structure

```
src/
├── main.rs              # Entry point, CLI parsing
├── app.rs               # Main loop, action dispatch, layout
├── action.rs            # Action enum (message passing)
├── event.rs             # Terminal event types
├── tui.rs               # Terminal setup, event loop
├── config.rs            # TOML config load/save
├── watcher.rs           # Filesystem watcher → repo index mapping
├── components/
│   ├── mod.rs           # Component trait
│   ├── repo_list.rs     # Left panel: repo list with status
│   ├── file_list.rs     # Middle panel: changed files + split diff
│   ├── git_graph.rs     # Right panel: commit graph + drill-down
│   ├── context_menu.rs  # Right-click overlay
│   ├── path_input.rs    # Add-repo input overlay
│   └── status_bar.rs    # Bottom bar with keybinding hints
└── git/
    ├── mod.rs
    ├── scanner.rs       # Repo discovery via walkdir
    ├── status.rs        # Branch, files, ahead/behind, fetch
    ├── graph.rs         # Lane-based commit graph builder
    ├── graph_render.rs  # Box-drawing character rendering
    └── commit_files.rs  # Commit file list and per-file diffs
```

## Related Projects

| Project | Description |
|---------|-------------|
| [**Fairtrail**](https://github.com/affromero/fairtrail) | Flight price evolution tracker with natural language search |
| [**PriceToken**](https://github.com/affromero/pricetoken) | Real-time LLM pricing API, npm/PyPI packages, and live dashboard |
| [**kin3o**](https://github.com/affromero/kin3o) | AI-powered Lottie animation generator CLI |

## License

[MIT](LICENSE)
