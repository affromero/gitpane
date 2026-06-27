#!/usr/bin/env bash
# Pre-commit hook: keep source files below the line-count limit so modules stay
# focused. Override the limit with GITPANE_MAX_FILE_LINES. Files already over the
# limit are grandfathered via the `exclude:` list in .pre-commit-config.yaml and
# should be split into focused modules over time, not grown.

set -euo pipefail

MAX_LINES="${GITPANE_MAX_FILE_LINES:-1000}"
EXIT=0

for f in "$@"; do
  [[ -f "$f" ]] || continue

  lines=$(wc -l < "$f")
  if (( lines <= MAX_LINES )); then
    continue
  fi

  printf '%s: %s lines exceeds the %s-line file limit. Split related code into a focused module.\n' \
    "$f" "$lines" "$MAX_LINES"
  EXIT=1
done

exit "$EXIT"
