# Changelog

All notable changes to gitpane are documented here.

## [0.3.1] - 2026-02-26

### Changed
- Help overlay (`?`) now shows context-aware keybindings per focused panel (Repos, Changes, Graph) with global keys always visible
- Added missing keybindings to help: `a` (add repo), `d` (remove), `s` (sort), `R` (rescan), `g` (git graph)

## [0.3.0] - 2025-02-25

### Added
- DAG-based branch collapse/expand (`c` key): computes branch segments from parent OIDs instead of visual lane positions, correctly handling lane reuse, interleaved commits, and unlabeled merged branches
- Main trunk protection: pressing `c` on the main branch no longer collapses the entire history
- Horizontal scroll for git graph (`h`/`l` keys + scroll wheel)
- Search/filter commits with `/` key, `n`/`N` to navigate matches
- First-parent mode toggle with `f` key
- Relative timestamps and deterministic author coloring
- Git tag display in graph labels (LightYellow color)
- Horizontal merge/fork lines and lane crossings in git graph
- Diff stats per commit (+N/-M) with async two-stage loading
- Dynamic row truncation to fit panel width
- `?` help overlay with keybinding reference
- Update checker with notification overlay
- Click branch labels to toggle branch visibility

### Fixed
- Diff view in changes panel now clears when files are staged (`git add`) or repo status changes
- Graph reloads no longer interrupt commit detail inspection
- Error/success messages clear after timeout

## [0.2.0] - 2026-02-24

### Added
- Branch labels on commit graph: colored tags on tip commits showing branch names
- Multi-branch graph walking: all branches visible, not just HEAD
- HEAD branch marked with green `*` prefix, worktree branches with magenta `⌂`
- Remote branches shown in red, local in cyan, comma-separated in parentheses
- Long branch names truncated with `…` (configurable `label_max_len`)
- `[graph]` config section: `branches` filter (`all`/`local`/`remote`/`none`) and `label_max_len`
- Merge commits rendered with dimmed message text (VS Code style)

### Fixed
- Clicking first item in Changes or Git Graph panels no longer triggers panel resize

## [0.1.3] - 2026-02-24

### Added
- Separate local (5s) and remote fetch (30s) poll timers, configurable via `watch.poll_local_secs` and `watch.poll_fetch_secs`

### Fixed
- Auto-refresh after commits, pulls, and checkouts (watch key `.git/` files like HEAD, index, refs/)
- Skip polling repos with active git operations to avoid conflicts

## [0.1.2] - 2026-02-24

### Added
- Drag panel borders with mouse to resize in both vertical and horizontal layouts
- Thick visual seam borders to signal draggable panel boundaries
- Linked worktree count per repo (`⎇N` indicator) for agentic AI workflows

### Fixed
- Eliminated idle CPU burn: render-on-demand instead of constant frame timer
- Fixed filesystem watcher feedback loop (`.git/` changes no longer re-trigger queries)
- Removed network fetch from watcher-triggered refreshes (local-only for speed)
- Replaced animated braille spinner with static `~` indicator for git ops
- Panel titles preserved when resizing in vertical layout mode

## [0.1.1] - 2026-02-24

### Added
- Spinner indicator during git push/pull/rebase operations
- Success toast message after git operations complete

### Fixed
- Sanitize git error messages to single line for status bar display
- Vendored OpenSSL for cross-platform builds

## [0.1.0] - 2026-02-24

### Added
- Three-panel TUI: Repositories, Changes, Git Graph
- Lane-based git commit graph visualization
- Mouse support: click to select, right-click context menus, scroll
- Double-click file to open split diff view
- Commit detail: click graph row to see files and diffs
- Add repos via `a` keybinding with path input and tab completion
- Remove repos with `d`, sort cycling with `s`
- Push/pull/rebase from right-click context menu
- Filesystem watching for live status updates
- Ahead/behind indicators with upstream tracking
- Copy to clipboard with `y` (OSC 52)
- Rescan repos with `R` to re-discover and clear exclusions
- CI/CD with GitHub Actions
