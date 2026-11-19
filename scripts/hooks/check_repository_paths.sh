#!/bin/sh
set -eu

# New top-level entries need an explicit review of the repository layout.
unexpected=$(git ls-files -- ':(top,glob)**' \
    ':(top,exclude,literal).github' \
    ':(top,exclude,literal).gitignore' \
    ':(top,exclude,literal).maintenance-exceptions.json' \
    ':(top,exclude,literal).pre-commit-config.yaml' \
    ':(top,exclude,literal)AGENTS.md' \
    ':(top,exclude,literal)CHANGELOG.md' \
    ':(top,exclude,literal)Cargo.lock' \
    ':(top,exclude,literal)Cargo.toml' \
    ':(top,exclude,literal)Justfile' \
    ':(top,exclude,literal)LICENSE' \
    ':(top,exclude,literal)README.md' \
    ':(top,exclude,literal)assets' \
    ':(top,exclude,literal)examples' \
    ':(top,exclude,literal)install.sh' \
    ':(top,exclude,literal)scripts' \
    ':(top,exclude,literal)src' \
    ':(top,exclude,literal)tests' \
    ':(top,exclude,literal)vscode-extension' \
    ':(top,exclude,literal)worktree-submodule-report.md')
hidden=$(git ls-files -- ':(top,glob)*/**/.*/**')

if [ -n "$unexpected$hidden" ]; then
    printf '%s\n' 'Unexpected tracked repository paths:' >&2
    [ -z "$unexpected" ] || printf '%s\n' "$unexpected" >&2
    [ -z "$hidden" ] || printf '%s\n' "$hidden" >&2
    exit 1
fi
