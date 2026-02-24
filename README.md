# gitpane

Multi-repo Git workspace dashboard for the terminal. Monitor all your repos at a glance with real-time status and a built-in commit graph.

Think "tmux for git repos" — see branch, dirty state, changed files, and commit history across your entire workspace without leaving the terminal.

## Features

- **Multi-repo overview** — Scans directories for git repos and displays branch, dirty indicator, and change count
- **Real-time updates** — Filesystem watcher triggers status re-queries within ~500ms of changes
- **Git graph** — Lane-based commit graph with colored box-drawing characters (press `g`)
- **File changes** — View changed files per repo with colored status indicators (M/A/D/R/?/C)
- **Mouse support** — Click to select, right-click for context menu, scroll wheel navigation
- **Configurable** — TOML config for root dirs, exclusions, pinned repos, scan depth
- **Responsive** — Collapses to single pane on narrow terminals

## Install

```bash
cargo install gitpane
```

Or build from source:

```bash
git clone https://github.com/afromero/gitpane.git
cd gitpane
cargo install --path .
```

## Usage

```bash
# Scan ~/Code (default)
gitpane

# Scan a specific directory
gitpane --root ~/projects

# Custom frame rate
gitpane --frame-rate 60
```

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate repos / scroll graph |
| `g` | Open git graph for selected repo |
| `r` | Refresh all repo statuses |
| `Esc` | Back to file list / quit |
| `q` | Quit |

### Mouse

| Action | Effect |
|--------|--------|
| Left click | Select repo |
| Right click | Context menu (graph, refresh, copy path) |
| Scroll wheel | Navigate list |

## Configuration

Place config at `~/.config/gitpane/config.toml`:

```toml
# Directories to scan for git repositories
root_dirs = ["~/Code"]

# Maximum directory depth for repo discovery
scan_depth = 2

# Always show these repos at the top
pinned_repos = ["~/Code/important-project"]

# Skip repos matching these patterns
excluded_repos = ["node_modules", ".cargo"]

[watch]
debounce_ms = 500

[ui]
frame_rate = 30
```

See [`examples/config.toml`](examples/config.toml) for a fully annotated example.

## Architecture

- **ratatui** + **crossterm** for the TUI
- **git2** (libgit2) for all git operations — no CLI parsing
- **notify** with debouncing for filesystem watching
- **tokio** async runtime with `spawn_blocking` for git queries

```
┌──────────────────────────────────────────────────┐
│ ┌─ Repositories ──┐ ┌─ Changes ────────────────┐ │
│ │ * main [3] myapp │ │  M src/main.rs           │ │
│ │   main    lib    │ │  A src/new_file.rs       │ │
│ │ * dev  [1] api   │ │  ? .env.example          │ │
│ │   main    docs   │ │                          │ │
│ └──────────────────┘ └──────────────────────────┘ │
│ [j/k] Navigate  [g] Graph  [r] Refresh  [q] Quit │
└──────────────────────────────────────────────────┘
```

## License

MIT
