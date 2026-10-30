"""Verify the files and runnable backend distributed in a Gitpane VSIX."""

import argparse
import json
import os
import subprocess
import tempfile
import tomllib
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

TARGETS = (
    "darwin-arm64",
    "darwin-x64",
    "linux-arm64",
    "linux-x64",
    "win32-x64",
)


def verify_payload(
    archive: Path, source: Path, backend: Path | None, target: str
) -> bytes:
    """Verify sources and identity; optionally compare a native built backend.

    Release preflight uses None for artifacts already checked on native CI hosts.
    Only that preflight accepts Git checkout CRLF/LF differences in known text.
    """
    executable = "gitpane.exe" if target.startswith("win32-") else "gitpane"
    backend_name = f"extension/bin/{executable}"
    files = {
        "extension/readme.md": source / "README.md",
        "extension/LICENSE.txt": source.parent / "LICENSE",
    }
    if backend is not None:
        files[backend_name] = backend
    for file in (source / "src").rglob("*"):
        if file.is_file():
            files[f"extension/{file.relative_to(source).as_posix()}"] = file
    expected = set(files) | {
        "[Content_Types].xml",
        "extension.vsixmanifest",
        "extension/package.json",
        backend_name,
    }
    with zipfile.ZipFile(archive) as package:
        names = package.namelist()
        if len(names) != len(set(names)) or set(names) != expected:
            raise ValueError(
                "VSIX contains missing, duplicate or unexpected files"
            )
        if package.testzip() is not None:
            raise ValueError("VSIX contains corrupt files")
        manifest = json.loads(
            (source / "package.json").read_text(encoding="utf-8")
        )
        if json.loads(package.read("extension/package.json")) != manifest:
            raise ValueError(
                "Packaged extension manifest does not match source"
            )
        identity = ET.fromstring(package.read("extension.vsixmanifest")).find(
            ".//{*}Identity"
        )
        if identity is None or identity.get("TargetPlatform") != target:
            raise ValueError(
                "VSIX target platform does not match the requested target"
            )
        for attribute, field in [
            ("Id", "name"),
            ("Publisher", "publisher"),
            ("Version", "version"),
        ]:
            if identity.get(attribute) != manifest[field]:
                raise ValueError(
                    "VSIX identity does not match the extension manifest"
                )
        for name, file in files.items():
            actual, expected = package.read(name), file.read_bytes()
            if backend is None and name.endswith(
                (".js", ".css", ".md", ".txt")
            ):
                actual = actual.replace(b"\r\n", b"\n")
                expected = expected.replace(b"\r\n", b"\n")
            if actual != expected:
                raise ValueError(
                    f"Packaged file does not match source: {name}"
                )
        if (
            not target.startswith("win32-")
            and not (package.getinfo(backend_name).external_attr >> 16) & 0o111
        ):
            raise ValueError("Packaged backend is not executable")
        data = package.read(backend_name)
        if not data:
            raise ValueError("Packaged backend is empty")
        return data


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--backend", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--run-integration", action="store_true")
    args = parser.parse_args()
    source = Path(__file__).resolve().parent.parent
    data = verify_payload(args.package, source, args.backend, args.target)
    metadata = tomllib.loads(
        (source.parent / "Cargo.toml").read_text(encoding="utf-8")
    )
    with tempfile.TemporaryDirectory(
        prefix="gitpane-vsix-check-"
    ) as directory:
        executable = Path(directory) / (
            "gitpane.exe" if args.target.startswith("win32-") else "gitpane"
        )
        executable.write_bytes(data)
        executable.chmod(0o755)
        result = subprocess.run(
            [str(executable), "--version"],
            check=True,
            capture_output=True,
            text=True,
        )
        expected = f"gitpane {metadata['package']['version']}"
        if result.stdout.strip() != expected:
            raise ValueError(
                f"Packaged backend version differs from {expected}"
            )
        print(
            f"Verified {args.target}: packaged files and {expected}",
            flush=True,
        )
        if args.run_integration:
            env = dict(os.environ, GITPANE_TEST_BINARY=str(executable))
            for name in ["bridge-real.js", "extension-real.js"]:
                subprocess.run(
                    ["node", str(source / "test/integration" / name)],
                    cwd=source.parent,
                    env=env,
                    check=True,
                )


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        ValueError,
        KeyError,
        ET.ParseError,
        zipfile.BadZipFile,
    ) as error:
        raise SystemExit(f"VSIX verification failed: {error}") from error
