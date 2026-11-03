---
slug: terminal-interaction
title: Terminal text selection and desktop sessions
status: done
created: 2026-11-02
---

## Intent

Gitpane supports selecting and copying visible terminal text while retaining mouse navigation, and opens or locates repository sessions in the official Herdr desktop client.

## Requirements

- [x] Dragging through panel text creates a visible text selection that can be copied, including reverse and multiline selections. Release copies; `y` or `Ctrl+C` copies highlighted text again; `Esc` clears it first.
- [x] Single clicks and the scroll wheel retain their behavior. Visible border and scrollbar cells retain their drag behavior; text cells take priority over the border's expanded hit zone. Modal overlays prevent interactions with content behind them.
- [x] Selection respects wide and combining Unicode characters and stays within the selected content pane.
- [x] Official `herdr-app` / `hc` sessions support repository opening, configured review commands, placement choices, live session markers, and jumping to a live terminal.
- [x] Desktop operations use explicit terminal and pane identities associated with the calling terminal, rather than another client's focused pane.
- [x] Existing tmux and legacy Herdr integrations retain their behavior under regression coverage.

## Non-goals

- Terminal emulator configuration changes.
- Changes to users' existing Herdr layouts or sessions during implementation verification.
- Changes to Git repository operations.
