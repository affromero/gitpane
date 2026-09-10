"""Test the distributed Cargo source archive, isolated from checkout artifacts."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile


def main():
    root = Path.cwd()
    metadata = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"],
        capture_output=True, text=True, check=True,
    )
    packages = json.loads(metadata.stdout)["packages"]
    package = next(item for item in packages
                   if Path(item["manifest_path"]).resolve() == root / "Cargo.toml")
    name = f"{package['name']}-{package['version']}"
    with tempfile.TemporaryDirectory(prefix="gitpane-source-package-") as directory:
        temporary = Path(directory)
        target = temporary / "target"
        # A fresh target prevents a checkout's test executable from being reused.
        env = dict(os.environ, CARGO_TARGET_DIR=str(target))
        subprocess.run(
            ["cargo", "package", "--locked", "--no-verify", "--allow-dirty"],
            env=env, check=True,
        )
        with tarfile.open(target / "package" / f"{name}.crate", "r:gz") as archive:
            archive.extractall(temporary / "source", filter="data")
        source = temporary / "source" / name
        if not (source / "Cargo.toml").is_file() or (source / ".git").exists():
            raise SystemExit("Package must contain Cargo.toml and no Git checkout")
        subprocess.run(
            ["cargo", "test", "--locked", "--all-targets", "--all-features"],
            cwd=source, env=env, check=True,
        )
        if os.name == "posix":
            subprocess.run(
                [sys.executable, str(root / "scripts/check_test_environment.py")],
                cwd=source, env=env, check=True,
            )


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout, file=sys.stderr)
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        raise SystemExit(error.returncode) from error
