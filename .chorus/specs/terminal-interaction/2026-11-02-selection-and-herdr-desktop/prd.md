---
title: Terminal text selection and official Herdr desktop compatibility
documentUuid: 4ce5792f-f17e-4f66-9ac9-fdfd2a35ab62
---

# Terminal text selection and official Herdr desktop compatibility

The user approved direct text drag selection while preserving click and wheel input, alongside official Herdr desktop integration. A mode-switch-only solution does not satisfy the selected interaction.

## Current behavior and evidence

`src/app/mod.rs` unconditionally enables mouse capture with `.mouse(true)`. `src/tui.rs` sends `EnableMouseCapture`, so ordinary pointer drags are delivered to Gitpane instead of terminal-native selection. Existing input handles panel clicks, scrollbars, and border drags; selected text can reuse `Action::CopyText` and the application's long-lived clipboard backend.

Existing session code recognizes `HERDR_*` and invokes legacy `herdr pane` / `herdr tab` commands. Installed official clients are `herdr-app 0.1.11` and `hc 0.1.16`; no legacy `herdr` executable is available. The official application's runtime skill identifies `$HC_TERMINAL_ID` as a terminal address, not a pane ID. `hc` owns terminal processes; `herdr-app` owns displayed spaces, tabs, and panes.

## Accepted behavior

Text-content drags establish a visible, pane-bounded selection. Release copies to the system clipboard; `y` and `Ctrl+C` copy highlighted text again, and `Esc` clears it first. Without a selection, existing copy and quit bindings remain. Normal clicks activate on release. Scrollbar and visible panel-boundary cells retain their drag behavior; text cells take priority over the border's expanded hit zone. Modal overlays cannot leak interaction into the underlying text. Copying handles forward, reverse, multiline and Unicode text. Selection is invalidated by content or geometry changes, navigation and overlays.

The official desktop integration covers default repository/worktree opening, configured review commands, right/below/new-tab placement and the placement picker, repository liveness, and jumping to live terminals. Operations resolve the calling terminal's actual desktop pane and space and use explicit identities. Existing tmux and legacy Herdr paths remain supported.

## Validation

Verified on Linux with `herdr-app 0.1.11`, `hc 0.1.16`, Rust 1.99 nightly, and Node 26.13. Formatting, build, Clippy with warnings denied, and rustdoc with warnings denied passed. JavaScript suite: 396 passed, 13 skipped, no failures.

The unfiltered Rust suite had 906 passing tests, one ignored test and one failure in `bridge::submodule::actions::tests::pull_latest_rejects_remote_or_pull_policy_changed_during_an_earlier_pull`. The production owner and test file are byte-identical to `HEAD`; the same failure was reproduced in a temporary `HEAD` export. Excluding only that pre-existing failure yielded 906 passing unit tests plus 12 passing CLI integration tests. The missing-Git environment check passed with 907 unit tests and 12 CLI tests, including expected missing-Git skips. The unrelated failing assertions were not changed.

Actual tmux TUI verification exercised single-click diff opening, drag highlight and clipboard round trip, reverse selection, Chinese and combining-character copy, `Ctrl+C` copy, wheel scrolling, scrollbar track jumping to the final diff row, and panel-border resizing. Clipboard connection was verified with the desktop display environment explicitly supplied to the test terminal.

Actual official desktop verification exercised `o` to a right split at a repository with spaces and Chinese characters, configured `v` output in a new tab, `ask` new-tab and below-source placements while another tab was focused, live markers and `G` navigation. Real editor bridge calls exercised explicit `herdr-app` session listing/liveness, launcher choices and execution with target tokens, and session navigation. Tests used temporary spaces and terminals created for verification. Screenshots and logs are local runtime evidence; no Windows or macOS runtime claims are made.

Final integration checks: `cargo fmt --all --check` and `node vscode-extension/test/integration/bridge-real.js` passed. `extension-real.js` failed at its unchanged line-698 assertion, “dirty worktree must be refused”; the same assertion failed with both source and binary built from a temporary `HEAD` export. This second unrelated baseline failure remains unchanged. A clean rebuild of the current checkout passed, followed by seven app-level text-selection tests and nine official-desktop backend tests. Verification-created spaces were closed and the prior displayed pane focus and clipboard text were restored.

The first independent mouse review found an internal Graph border halo still rejecting neighboring message text. A new app regression failed before the correction, then passed for both 160- and 90-column layouts after removing that text exclusion. The actual TUI now highlights and copies `Initial` from the first Message content row without moving the border. All eight app selection regressions pass, including the retained actual-border drag test. After this correction: 907 unit tests and 12 CLI tests pass with only the known Rust baseline failure excluded; the missing-Git gate passes with 908 unit tests and 12 CLI tests. Build, formatting, Clippy and rustdoc warnings-denied checks pass.

The first official-desktop review found wrapper/extension workers could lose inner-tmux ownership. Detection now walks at most 64 process ancestors and stops at the HC terminal root. Regression coverage checks an extension-like chain, an outer tmux beyond that root, direct execution, and cycles. A real tmux pane with an extra worker wrapper and bounded desktop-membership fixtures reproduced `HerdrApp` with the pre-fix binary and `Tmux` with the corrected binary; only the desktop API boundary was simulated in this nesting probe. Official launch/focus smoke above used the real installed desktop. Bridge failures also retain official CLI JSON error reasons from stdout rather than showing an empty stderr message.

After the nested-worker and error-reason corrections, final verification logs `/tmp/gitpane-herdr-review-fix-{0..5}.log` record: build, Clippy, rustdoc and formatting checks passed; 909 Rust unit tests and 12 CLI tests passed with only the proven pre-existing submodule test excluded; the missing-Git run passed with 910 unit tests and 12 CLI tests. One manual environment probe remains ignored in the normal suite.

## Tracking

Chorus project: `c01ab4f3-2f76-4493-b601-6c38ab5fbaea`.

- Text selection: `ec794970-51f6-4ba5-9b17-415d600eb6e6`.
- Official desktop compatibility: `45f210f6-fdae-450a-8844-7a3dd9273ccf`.

The two standalone implementation tasks use the Quick Dev workflow; this change has no proposal container.

Both tasks are verified `done` after their independent round-2 `VERDICT: PASS` comments: mouse `de922b75-140e-482c-93b9-c6462dd5ce34`; official desktop `23044a2f-4eb6-429a-8aab-7c46c9bf88ea`. There is no Idea-rooted proposal, so no aggregate Idea review gate applies; spec-lite has no OpenSpec change to archive. No commits or pushes were made.
