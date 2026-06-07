# Changelog

All notable changes to gitpane are documented here.

## [0.7.13] - 2026-06-07

### Fixed
- Adding a git submodule or linked worktree with the `A` key no longer removes it from the list almost immediately. A submodule stores its `.git` as a pointer file (`gitdir: <path>`) instead of a directory, so repository discovery did not recognize it as a real repo and pruned the pinned entry on the next filesystem rescan. Discovery now resolves the `gitdir:` pointer (absolute, or relative to the working tree) and looks for `HEAD` at the referenced git directory, so pinned submodules and worktrees persist.

## [0.7.12] - 2026-06-05

### Fixed
- The commit graph now refreshes when a commit or push lands on a branch other than the selected repository's checked-out HEAD, including commits made in linked worktrees whose branches share the repository's refs. Previously the change check only looked at the checked-out branch, so a commit on any other branch (a common case when several worktrees commit locally) left the graph stale until you switched repositories and back. The reload still fires only when a rendered ref actually moves, so it adds no extra graph rebuilds on ordinary file edits.

## [0.7.11] - 2026-06-03

### Fixed
- The commit graph no longer freezes for a repository when a background graph build aborts unexpectedly. A guard now releases the in-flight load latch and replays any coalesced reload, so the graph keeps updating on commit and push instead of going stale until you switch repositories and back. Repeatedly failing builds are capped with bounded auto-retry so they cannot rebuild on every file-watcher event, and a stale build result can no longer overwrite a freshly loaded graph.

## [0.7.10] - 2026-06-01

### Added
- Build-time version override via the optional `GITPANE_BUILD_OVERWRITE_VERSION` environment variable, so packagers can stamp a custom version such as a git commit hash into the reported version. Empty values fall back to the Cargo package version, and `gitpane update` still compares against the base version. The new `--version` / `-V` flags print it. Thanks to @0x61nas.
- Clearer messaging when the `git` binary is not available on the PATH.

### Fixed
- The gitignore watcher test no longer requires the `git` CLI, so the suite passes in pure build environments without git installed (reported by @DoomHammer).
- Change badges now render on worktree rows.

## [0.7.9] - 2026-05-31

### Changed
- Tightened the minimum `tokio` and `tracing-subscriber` versions to patched releases so dependency status tools do not report vulnerable semver ranges.

## [0.7.8] - 2026-05-31

### Added
- Public library target and docs.rs metadata so published package docs build with warnings denied.
- Docs workflow plus a pre-push `cargo doc` hook.

### Changed
- Updated direct dependencies `git2`, `strum`, and `toml`, clearing the outdated direct dependency set reported by deps.rs.
- README badges are grouped into separate check, package health, and project metadata rows with matching Shields styling.

### Fixed
- Adapted graph and status code for the `git2` 0.21 shorthand and summary APIs.

## [0.7.7] - 2026-05-30

### Added
- New `gitpane diagnostic` command prints a copyable report with version, platform, config source, scan roots, watcher settings, graph settings, discovered repo count, missing roots, and warnings for settings that can raise CPU usage. `gitpane diagnostics` is accepted as an alias.

### Changed
- Nested worktree directory watching is now opt in via `[watch] watch_worktree_dirs = true`. The default is `false`, so Linux inotify stays quiet in workspaces with busy generated or training files. Git metadata and repo roots are still watched, and the existing local poll continues to catch nested file changes.
- Watcher triggered status refreshes are capped by `[watch] refresh_cooldown_ms`, default `5000`, so repeated file events cannot start back to back status walks for the same repo.
- Background local status checks no longer recursively expand untracked directories. Explicit refresh with fetch keeps the deeper untracked listing behavior.
- The TUI no longer redraws every 250 ms while idle. Ticks now handle lightweight message expiry, and renders are driven by input, polling, watcher changes, resize, and status updates.

### Fixed
- Busy Linux workspaces no longer peg a full core in `notify-rs inoti` when tracked files are written continuously under large repos. On the repro server, current CPU dropped from roughly one saturated core to about 10 to 16 percent, and in flight watcher refresh markers dropped to single digits over a 90 second sample.
- Repo status updates that only change the file list no longer reload the commit graph. Graph reloads are limited to branch, HEAD, ahead or behind, and stash changes, with an in flight guard to coalesce overlapping graph loads.
- TUI redraws now start from a blank buffer, and tracing is silent by default unless `GITPANE_LOG`, `RUST_LOG`, or `GITPANE_LOG_FILE` is set. This prevents stale panel titles/content and info log lines from appearing inside the alternate-screen UI.

## [0.7.6] - 2026-05-30

### Fixed
- Startup no longer hangs on large checkouts. The filesystem watcher walked every directory under each repo (honoring only the name-based `watch_exclude_dirs`), so it descended gitignored trees like `data/`, `.venv/`, and `__pycache__/`. On an ML workspace that is over a million files and tens of thousands of inotify watches, all installed synchronously before the UI could paint. The watcher now walks with the `ignore` crate, respecting `.gitignore`, `.git/info/exclude`, and the global gitignore, so ignored trees are never walked or watched (changes inside them never affect `git status` anyway). The repo's own `.git` is pruned from the working-tree walk and re-watched selectively (its top-level files plus `refs/`) so commit, checkout, and branch updates still trigger a refresh. On one workspace this cut the walk from roughly 1.14M files to 4.4k, and watches from about 13.7k to 779.
- `Ctrl+C` now quits gitpane. It was bound nowhere, so only `q` exited; combined with the blocking startup walk above, a slow launch looked like a process that ignored `Ctrl+C` (keystrokes were buffered but never processed, and there was no `Ctrl+C` binding even once they were). Raw mode clears the terminal's `ISIG`, so `Ctrl+C` arrives as a key event rather than `SIGINT`; it is now handled globally, ahead of any overlay or panel routing.

### Changed
- The filesystem watcher is now built on a background thread instead of synchronously during launch, so the UI is interactive immediately even while watches are still being installed. The periodic local poll covers the brief window before the watcher comes online. A `filesystem watcher ready: N repos in <elapsed>` line is logged at info level (visible via `GITPANE_LOG` / `GITPANE_LOG_FILE`).

## [0.7.5] - 2026-05-29

### Fixed
- Repo discovery now ignores empty `.git` directories (no `HEAD` file). A bare `mkdir .git` left over from an aborted clone, or any other stray empty `.git/` placeholder, would otherwise get treated as a real repo: every file change anywhere under the parent (commonly the configured `root_dirs` itself, e.g. `~/Code/`) routed to that "repo" via the watcher's classifier, `Repository::open` failed on each status query, and the status bar filled with an infinite stream of red `Failed to query: NotFound` toasts. The phantom is now skipped at discovery time, both for pinned repos and for root-dir scanning.

### Added
- `GITPANE_LOG_FILE=/path/to/log` redirects all `tracing` output to a file instead of stderr. Useful for capturing watcher warnings and `Action::Error` toast text while the TUI is occupying the screen. `GITPANE_LOG=...` is also accepted as a namespaced alias for `RUST_LOG`.
- `just deploy-remote` and `just run-remote` recipes: rsync the source to an ssh host, build there, install over `~/.cargo/bin/gitpane`, and (for `run-remote`) launch in a detached tmux pane with `GITPANE_LOG_FILE` set, then print the captured log. Override `HOST=` / `REMOTE=` / `SECS=` on the command line. Intended for fast iteration against a workstation without going through a release cycle.

## [0.7.4] - 2026-05-28

### Fixed
- Watcher no longer descends into symlinks (e.g. a Wine prefix's `dosdevices/z:` -> `/`), so it stops emitting permission warnings for root-owned paths like `/tmp/systemd-private-*` that happen to live under a tracked repo. The watcher walks each repo itself with walkdir, installing one non-recursive notify watch per real directory while skipping symlinks and `watch_exclude_dirs` entries before they ever hit inotify.
- Deleting a tracked repo (e.g. `rm -rf <repo>` or `rm -rf <repo>/.git`) no longer surfaces a `Failed to query: ...` toast. The watcher-triggered `RefreshRepo` now detects the missing `.git` and fires `DiscoverNewRepos` instead, so the repo simply drops out of the list.

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
