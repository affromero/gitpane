#!/usr/bin/env python3
"""Validate release metadata and extract its changelog (requires Python 3.11+)."""

import argparse
from pathlib import Path
import re
import sys

if sys.version_info < (3, 11):
    sys.exit("Release validation requires Python 3.11 or newer.")

import tomllib


def release_notes(root: Path, tag: str) -> str:
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", tag):
        raise ValueError("release tag must have the form vX.Y.Z")
    version = tag[1:]
    with (root / "Cargo.toml").open("rb") as manifest_file:
        package = tomllib.load(manifest_file)["package"]
    if package.get("name") != "gitpane" or package.get("version") != version:
        raise ValueError(f"Cargo.toml must declare gitpane version {version}")
    with (root / "Cargo.lock").open("rb") as lock_file:
        packages = tomllib.load(lock_file)["package"]
    locked = [package for package in packages if package.get("name") == "gitpane"]
    if len(locked) != 1 or locked[0].get("version") != version:
        raise ValueError(f"Cargo.lock must contain exactly one gitpane package at version {version}")

    lines = (root / "CHANGELOG.md").read_text(encoding="utf-8").splitlines()
    starts = []
    for index, line in enumerate(lines):
        heading = re.fullmatch(r"## \[([^\]]+)\](?:[ \t].*)?", line)
        if heading and heading[1] == version:
            starts.append(index)
    if len(starts) != 1:
        raise ValueError(f"CHANGELOG.md must contain exactly one ## [{version}] section")
    start = starts[0]
    end = next(
        (index for index in range(start + 1, len(lines))
         if re.match(r"^##(?:[ \t]|$)", lines[index])),
        len(lines),
    )
    body = lines[start + 1:end]
    if not any(line.strip() and not re.match(r"^#{1,6}(?:[ \t]|$)", line) for line in body):
        raise ValueError(f"CHANGELOG.md section {version} has no release notes")
    return "\n".join(lines[start:end]).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--notes", type=Path, help="Write validated notes to this file")
    args = parser.parse_args()
    try:
        notes = release_notes(args.root, args.tag)
        if args.notes:
            args.notes.write_text(notes, encoding="utf-8")
        else:
            print(notes, end="")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Release validation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
