#!/bin/sh
set -eu
unset TMUX TMUX_PANE
trap 'tmux -L "$GITPANE_DEMO_SOCKET" kill-server 2>/dev/null || :' 0

tmux -L "$GITPANE_DEMO_SOCKET" -f /dev/null new-session -d \
  'exec env -u NO_COLOR GITPANE_CONFIG="$GITPANE_DEMO_CONFIG" "$GITPANE_DEMO_BINARY" --root "$GITPANE_DEMO_ROOT" --no-sync-repos'
tmux -L "$GITPANE_DEMO_SOCKET" set-option -g status off
(
  sleep 30
  tmux -L "$GITPANE_DEMO_SOCKET" split-window -d -h -l 45% \
    'printf "\n  Responsive layout\n"; sleep 7'
) &
tmux -L "$GITPANE_DEMO_SOCKET" attach-session
