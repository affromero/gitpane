"""Publish the complete set of CI-verified platform packages using GitHub OIDC."""

import argparse
import subprocess
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

from check_vsix import TARGETS, verify_payload


def publish_packages(directory: Path, source: Path, *, verify_only: bool) -> None:
    packages = [
        directory / f"gitpane-explorer-{target}.vsix" for target in TARGETS
    ]
    if set(directory.iterdir()) != set(packages):
        raise ValueError("Release requires exactly the five platform VSIX files")
    for target, package in zip(TARGETS, packages, strict=True):
        verify_payload(package, source, None, target)
        print(f"Verified Marketplace package: {target}", flush=True)
    if verify_only:
        return
    try:
        subprocess.run(
            [
                "npx", "--yes", "@vscode/vsce@4.1.0", "publish", "--oidc",
                "--packagePath", *(str(package.resolve()) for package in packages),
            ],
            cwd=source,
            check=True,
        )
    except subprocess.CalledProcessError as error:
        raise ValueError(
            "Marketplace publication failed. Earlier platform targets may already "
            "be public. Inspect the publisher before retrying; this workflow does "
            "not skip existing versions or roll back published targets."
        ) from error


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    publish_packages(
        args.artifacts,
        Path(__file__).resolve().parent.parent,
        verify_only=args.verify_only,
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, ET.ParseError, zipfile.BadZipFile) as error:
        raise SystemExit(f"Marketplace release failed: {error}") from error
