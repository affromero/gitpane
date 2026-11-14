#!/bin/sh
# Populate an empty, caller-owned temporary directory with sample repositories.
set -eu

root=${1:?Pass an empty temporary directory}
mkdir -p "$root/workspace" "$root/config"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME='Alex Morgan' GIT_AUTHOR_EMAIL='alex@example.com'
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME" GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"

for name in atlas-api meadow-web shared-ui; do
  repo="$root/workspace/$name"
  mkdir -p "$repo/src"
  git -C "$repo" init -q -b main
  git -C "$repo" config core.hooksPath /dev/null
  printf '# %s\n\nA sample project for the Gitpane demo.\n' "$name" > "$repo/README.md"
  printf 'export const app = "%s";\n' "$name" > "$repo/src/app.ts"
  git -C "$repo" add .
  git -C "$repo" commit -q -m 'Initialize project'
  printf '\nexport const ready = true;\n' >> "$repo/src/app.ts"
  git -C "$repo" add .
  git -C "$repo" commit -q -m 'Add application status'
done

repo="$root/workspace/atlas-api"
git -C "$repo" switch -q -c feature/search
printf '\nexport const search = (query: string) => query.trim();\n' >> "$repo/src/app.ts"
git -C "$repo" add src/app.ts
git -C "$repo" commit -q -m 'Add search endpoint'
printf '\nexport const maxResults = 100;\n' >> "$repo/src/app.ts"
git -C "$repo" add src/app.ts
git -C "$repo" commit -q -m 'Limit search results'
printf '\nexport const apiVersion = "v1";\n' >> "$repo/src/app.ts"
git -C "$repo" add src/app.ts
git -C "$repo" commit -q -m 'Version the API'

# Exercise the same Git metadata that Gitpane reads in a real workspace.
git -C "$repo" worktree add -q -b feature/filters "$root/atlas-filters"
printf '\nexport const filters = true;\n' >> "$root/atlas-filters/src/app.ts"
git -C "$repo" -c protocol.file.allow=always submodule add -q "$root/workspace/shared-ui" packages/shared-ui
git -C "$repo" commit -qm 'Add shared UI submodule'
printf '\nexport const theme = "ocean";\n' >> "$repo/packages/shared-ui/src/app.ts"
printf '\nexport const draftSearch = true;\n' >> "$repo/src/app.ts"
git -C "$repo" add src/app.ts
printf '\nexport const draftLimit = 20;\n' >> "$repo/src/app.ts"
git -C "$repo" stash push -qm 'Draft search improvements'
printf '\nexport const pageSize = 25;\n' >> "$repo/src/app.ts"
printf 'export const timeoutMs = 5000;\n' > "$repo/src/config.ts"
git -C "$repo" add src/config.ts
mkdir -p "$repo/tests"
printf 'Search should ignore surrounding whitespace.\n' > "$repo/tests/search.test.ts"
printf '# Sample Gitpane configuration\n' > "$root/config/config.toml"
