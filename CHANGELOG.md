# Changelog

All notable changes to gitpane are documented here.

## [0.7.3] - 2026-05-23

### Added
- Repo list auto-rescans when a new git repository appears or vanishes under any configured `root_dirs`, so a fresh `git clone ~/Code/foo` shows up automatically without pressing `R`. Each configured root is watched with a non-recursive notify watch; events that match the direct child of a root (and are not already inside a known repo) fire an idempotent `DiscoverNewRepos` action.
- New `[watch] discovery_cooldown_secs` config key (default `5`) coalesces FS event storms during `git clone` so the rescan does not thrash; a single trailing-edge fire still picks up repos finishing mid-burst.
- Git graph now renders `stash@{n}` labels on the commit each stash was created on, themed via the new `graph.stash_label` color so stashes read consistently across the repo list and the graph. Orphaned stashes (parent commit no longer reachable from any branch) still appear because the parent oid is pushed into the revwalk as its own seed.

### Fixed
- Pressing `R` (manual `RescanRepos`) now also rebuilds the filesystem watcher, so newly discovered repos actually get watched. Previously the watcher was created once at startup and held as a function-local binding, so post-rescan repos went unwatched until restart.
- Clicking the stash chevron toggles the stash subtree even when the repo also has worktrees. The mouse handler now hit-tests `mouse.column` against each indicator's range, routing the click to whichever chevron was clicked. Clicks elsewhere on the row keep the previous worktree-first fallback.
- `rebuild_watcher` keeps the previous watcher alive when constructing a new one fails, instead of silently going unwatched after a transient notify error.

## [0.7.2] - 2026-05-22

### Fixed
- Graph worktree marker (`⌂` prefix on worktree branch labels) now matches the repo-list worktree color: amber (`Color::Indexed(214)`) in the default theme, Orange3 (`Color::Indexed(172)`) in the muted preset. Previously the graph kept the pre-0.7.1 `Magenta` (default) and `Indexed(133)` (muted) values, so the same worktree showed up amber in the repo list and magenta in the graph.

## [0.7.1] - 2026-05-21

### Added
- In-app theme picker: press `t` from any panel to open a modal list of every available theme (built-in + custom), preview live as you cycle, Enter to persist, Esc to revert
- `gitpane --theme <name>` CLI flag to override the active theme for a single run without touching `config.toml`
- `gitpane themes` subcommand prints all available themes with a marker on the currently-resolved one
- Expandable stash subtree on the repo row: press `S` to toggle a `▶/▼ $N` chevron and list each `stash@{n}` underneath, mirroring the existing worktree UX. Click on an already-selected repo with stashes (and no worktrees) also toggles

### Changed
- Default-theme color split: worktree indicators move from `Magenta` to `Color::Indexed(214)` amber/gold; stash moves from `Color::Indexed(67)` steel blue (too close to the Cyan branch) to `Color::Indexed(127)` dark magenta
- `muted` preset tracks the new color split (Orange3 for worktree, Plum4 for stash)
- Release workflow now extracts the per-version `CHANGELOG.md` section and passes it to GitHub Release as the body, so future releases never ship with an empty body

## [0.7.0] - 2026-05-21

### Added
- Stash indicator on the repo row (`$N`) when a repo has stashed work; legend entry next to the existing markers
- Themeable colors: every UI color now reads from a centralized `Theme`. Pick a built-in via `theme = "default"` or `theme = "muted"` in `config.toml`, or set `theme = "<name>"` to load a custom file from `<config_dir>/gitpane/themes/<name>.toml`
- `muted` preset built for dark terminals where the default `Light*` colors feel too bright

### Fixed
- Repo list now counts ahead commits for branches without an upstream (freshly-created local branches with remote refs present), matching `git log HEAD --not --remotes` semantics
- Submodule unpushed-commit counts also benefit from the new no-upstream walk

## [0.6.2] - 2026-05-09

### Fixed
- Submodule status tests no longer depend on the host's git config or default branch name — they now run reliably in pure CI environments

## [0.6.1] - 2026-05-08

### Fixed
- Config files under `$XDG_CONFIG_HOME/gitpane/config.toml` and `~/.config/gitpane/config.toml` are now discovered on every platform before falling back to the platform-native config path
- `GITPANE_CONFIG` now works as an exclusive config-file override and save target
- Config saves now write back to the loaded file, or to the selected override/default path when starting from built-in defaults

### Documentation
- Documented config lookup order, save behavior, and startup config-path logging

## [0.6.0] - 2026-05-07

### Added
- Warn when a submodule pointer or commits are unpushed: surfaces the classic footgun where a parent commit pins a submodule oid no remote can resolve
- File row tag `[sub: +commit ⚠unreach]` / `[sub: ↑N]` composes orthogonally with the existing dirty/modified/uninitialized states
- Red `⇡` glyph on the repo row alongside the existing `◈` dirty-submodule marker
- Status-bar legend entry for `⇡` next to the push/pull symbols
- Config toggle `[submodules] warn_unpushed = false` to silence the warning (default `true`); independent of `ignore_dirty`

### Changed
- Detection is local-only: submodule branch ahead-of-upstream count plus reachability of the parent's recorded oid from any of the submodule's `refs/remotes/*` refs — no extra `git fetch` per submodule
- `SubmoduleInfo.state` is now `Option<SubmoduleState>`; a submodule may appear in the list because of a warn signal even when it has no dirty/modified state
- Dropped the long-unused `_ignore_dirty_subs` argument from `RepoList::new`

## [0.5.3] - 2026-04-18

### Fixed
- Git graph now live-reloads while a worktree row is selected — new commits made inside the worktree appear on the next `PollLocal` tick instead of requiring a manual re-select

## [0.5.2] - 2026-04-14

### Added
- Worktree changes now update in real time — `PollLocal` re-queries the active worktree on each tick so file modifications appear automatically in the Changes panel

## [0.5.1] - 2026-04-13

### Fixed
- Clicking an already-selected repo row now toggles worktree expansion (previously only the `w` key worked)
- Worktree changes no longer flash and disappear — repo status polls no longer overwrite the Changes panel while a worktree is being viewed

## [0.5.0] - 2026-04-13

### Added
- Collapsible worktree entries in the repo list: repos with linked worktrees show a `▶N` / `▼N` toggle icon — press `w` to expand and see each worktree branch listed below the parent repo
- Selecting a worktree entry shows its changed files in the Changes panel with full diff support
- Git graph loads from the selected worktree path for branch-specific history
- `gitpane update` self-update subcommand for easy in-place upgrades

### Changed
- Worktree data now includes full details (name, path, branch) collected via the git2 API, replacing the previous simple count

## [0.4.1] - 2026-04-01

### Added
- Fetch failure indicator: repos show a dim `⚠` when `git fetch` fails (auth, network, timeout), so stale ahead/behind counts are visible

### Fixed
- Repos no longer get stuck with a permanent spinner when a status query fails (`git_op` is now cleared on error via `StatusQueryDone`)
- RAII `StatusGuard` ensures `pending_status` is cleaned up even if a spawned task panics
- RAII `GitOpGuard` ensures push/pull/submodule operations reset `git_op` on panic
- Rapid file/commit navigation no longer shows stale diffs (generation counters on `DiffLoaded`, `CommitFilesLoaded`, `CommitDiffLoaded`)
- Background `git fetch` now has a 30-second timeout — hung remotes no longer block all polls indefinitely
- Startup status queries now go through the shared semaphore, preventing unbounded thread bursts on launch

### Changed
- Repos are now identified by path (`RepoId`) instead of positional index, fixing a class of bugs where add/remove/sort/rescan could apply status to the wrong repo or corrupt tracking state
- Watcher uses path-based routing — no longer needs rebuilding when repos are added or removed
- `RescanRepos` clears `pending_status`/`dirty_repos`; `RemoveRepo` cleans up tracking sets

## [0.4.0] - 2026-03-18

### Added
- Full dirty submodule awareness: detects Modified (pointer changed), Dirty (uncommitted local changes), and Uninitialized states
- `◈` badge in repo list when any submodule has a non-clean state
- `[sub: +commit]`, `[sub: ~dirty]`, `[sub: -uninit]` annotations in the file list with LightMagenta styling
- Submodule-aware diff panel: shows `git diff` for dirty workdirs, commit log for pointer changes
- Context menu actions: "Sub: update --init", "Sub: sync", "Sub: pull latest"
- Config toggle `[submodules] ignore_dirty = true` to suppress submodule noise entirely

## [0.3.11] - 2026-03-16

### Fixed
- Clicking files in the commit detail panel did nothing (click coordinates were offset by the message block height)
- Commit message in detail panel was not scrollable when longer than the visible area
- Commit message block now always shows at least one line, even in small terminals

## [0.3.10] - 2026-03-16

### Added
- "Pull --recurse-submodules" option in context menu, shown only for repos that have submodules

## [0.3.9] - 2026-03-16

### Added
- Full commit message displayed in detail panel when clicking a commit in the git graph (previously only showed OID and file list)

## [0.3.8] - 2026-03-14

### Fixed
- Graph not updating after `git gc` or packed-ref operations (watcher now monitors `.git/packed-refs`)
- Graph not refreshing when switching back to gitpane from another terminal (FocusGained now triggers a refresh)
- Graph staying stale after closing commit details when data arrived while the detail panel was open
- Stale graph results from a previous repo overwriting the current view on fast repo switching (generation counter guards)
- Watcher dedup dropping changes that arrived during an in-flight status query (dirty-repo retry on completion)

## [0.3.7] - 2026-03-13

### Added
- Adaptive split direction for diff/detail panels: inner splits use vertical (top/bottom) layout when the terminal is wide enough for side-by-side panels, preventing unreadably cramped content

## [0.3.6] - 2026-03-12

### Fixed
- Git graph panel was blank after the 0.3.5 performance optimisation: the render loop was still reading from `self.rows` (cleared by the fast path) instead of `display_rows()`, producing zero list items

## [0.3.5] - 2026-03-11

### Fixed
- High CPU usage when monitoring many repos: poll operations now limited to 4 concurrent tasks via semaphore
- Duplicate status queries for the same repo are skipped when one is already in-flight
- Watcher no longer triggers refreshes for events inside node_modules, target, .build, dist, vendor, .venv, __pycache__, .next, or Pods directories
- Git graph no longer clones ~200 rows on every refresh when no branches are collapsed

### Documentation
- Added all keybindings to README
- Documented all configuration parameters

## [0.3.4] - 2026-03-02

### Added
- Confirmation dialog when removing a repo (`d` key) to prevent accidental removal

### Fixed
- Release badge now resolves correctly (switched from github/v/release to github/v/tag)
- Added crates.io downloads badge to README
- Updated README test count (17 → 90)

## [0.3.2] - 2026-02-26

### Added
- Version display in status bar

### Changed
- Improved VHS demo recordings for readability

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
