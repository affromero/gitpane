"""Run the Rust test suite without Git on PATH (Linux and macOS)."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    if os.name != "posix":
        raise SystemExit("This check requires a Unix environment")

    build = subprocess.run(
        ["cargo", "test", "--locked", "--all-targets", "--all-features",
         "--no-run", "--message-format=json-render-diagnostics"],
        stdout=subprocess.PIPE, text=True, check=True,
    )
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.strip()]
    tests = [
        item for item in artifacts
        if item.get("reason") == "compiler-artifact"
        and item.get("profile", {}).get("test")
        and item.get("executable")
    ]
    # A library-only run succeeds with zero tests in this project.
    if not any(item["target"]["name"] == "gitpane"
               and item["target"]["kind"] == ["bin"] for item in tests):
        raise SystemExit("Cargo did not produce the gitpane binary test harness")

    with tempfile.TemporaryDirectory(prefix="gitpane-test-path-") as directory:
        # Process-group tests still need a shell and a sleeping grandchild.
        for name in ("sh", "sleep"):
            executable = shutil.which(name)
            if executable is None:
                raise SystemExit(f"Required test utility missing: {name}")
            Path(directory, name).symlink_to(Path(executable).resolve())
        env = dict(os.environ, PATH=directory)
        if shutil.which("git", path=directory) is not None:
            raise SystemExit("Git must be absent from the restricted test PATH")

        for item in tests:
            executable = item["executable"]
            print(f"Testing without Git: {executable}", flush=True)
            # Change only the child's PATH. Cargo and the caller keep theirs.
            subprocess.run([executable, "--nocapture"], env=env, check=True)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from error
