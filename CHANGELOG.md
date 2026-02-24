# Changelog

All notable changes to gitpane are documented here.

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
