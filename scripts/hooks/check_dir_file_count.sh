#!/usr/bin/env bash
# Pre-commit hook: keep source directories focused — at most N source files per
# directory, so a broad folder gets split into subdirectories instead of
# sprawling. Override the cap with GITPANE_MAX_FILES_PER_DIR.

set -euo pipefail

max_files="${GITPANE_MAX_FILES_PER_DIR:-10}"

violations=$(
  git ls-files 'src/*.rs' | awk -F/ -v max_files="$max_files" '
    {
      dir = $1
      for (i = 2; i < NF; i++) dir = dir "/" $i
      counts[dir]++
    }
    END {
      for (dir in counts)
        if (counts[dir] > max_files)
          printf "%s: %d files\n", dir, counts[dir]
    }
  ' | sort
)

if [ -n "$violations" ]; then
  echo "Source directories may contain at most ${max_files} files." >&2
  echo "Split broad folders into focused subdirectories:" >&2
  echo "$violations" >&2
  exit 1
fi
