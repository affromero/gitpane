"""Behavioral checks for the distributed VSIX payload."""

import json
import struct
import subprocess
import tempfile
import unittest
import zipfile
import zlib
from collections.abc import Mapping
from pathlib import Path
from unittest.mock import patch

from check_vsix import TARGETS, verify_payload
from publish_marketplace import publish_packages


def png_image(width: int, height: int) -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    pixels = (b"\x00" + b"\xff\x00\x00" * width) * height
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


class PackageTests(unittest.TestCase):
    root: Path
    source: Path
    backend: Path
    archive: Path

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "vscode-extension"
        (self.source / "src").mkdir(parents=True)
        (self.source / "media").mkdir()
        (self.source / "media/gitpane.png").write_bytes(png_image(256, 256))
        manifest = {
            "name": "gitpane-explorer",
            "publisher": "gitpane",
            "version": "0.0.1",
            "icon": "media/gitpane.png",
        }
        (self.source / "package.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )
        (self.source / "README.md").write_bytes(b"Gitpane Explorer\n")
        (self.source / "src/view.js").write_bytes(b"visible sources\n")
        (self.root / "LICENSE").write_bytes(b"license\n")
        self.backend = self.root / "gitpane"
        self.backend.write_bytes(b"built backend")
        self.archive = self.root / "package.vsix"

    def package(
        self,
        changes: Mapping[str, bytes | None] | None = None,
        target: str = "darwin-arm64",
        mode: int = 0o100755,
    ) -> None:
        contents = {
            "[Content_Types].xml": b"types",
            "extension.vsixmanifest": (
                "<PackageManifest "
                'xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">'
                '<Metadata><Identity Id="gitpane-explorer" '
                'Publisher="gitpane" Version="0.0.1" '
                f'TargetPlatform="{target}"/></Metadata>'
                '<Assets><Asset '
                'Type="Microsoft.VisualStudio.Services.Icons.Default" '
                'Path="extension/media/gitpane.png"/>'
                '</Assets></PackageManifest>'
            ).encode(),
            "extension/package.json": (
                self.source / "package.json"
            ).read_bytes(),
            "extension/readme.md": (self.source / "README.md").read_bytes(),
            "extension/LICENSE.txt": (self.root / "LICENSE").read_bytes(),
            "extension/src/view.js": (
                self.source / "src/view.js"
            ).read_bytes(),
            "extension/media/gitpane.png": (
                self.source / "media/gitpane.png"
            ).read_bytes(),
            "extension/bin/gitpane.exe"
            if target.startswith("win32-")
            else "extension/bin/gitpane": self.backend.read_bytes(),
        }
        for name, data in (changes or {}).items():
            if data is None:
                contents.pop(name)
            else:
                contents[name] = data
        with zipfile.ZipFile(self.archive, "w") as package:
            for name, data in contents.items():
                info = zipfile.ZipInfo(name)
                info.external_attr = mode << 16
                package.writestr(info, data)

    def test_verified_payload_returns_the_exact_built_backend(self) -> None:
        for target in [
            "darwin-arm64",
            "darwin-x64",
            "linux-arm64",
            "linux-x64",
            "win32-x64",
        ]:
            with self.subTest(target=target):
                self.package(
                    target=target,
                    mode=0o100644 if target.startswith("win32-") else 0o100755,
                )
                self.assertEqual(
                    verify_payload(
                        self.archive, self.source, self.backend, target
                    ),
                    b"built backend",
                )

    def test_altered_and_incomplete_packages_are_rejected(self) -> None:
        for changes in [
            {"extension/src/view.js": b"stale sources"},
            {"extension/media/gitpane.png": b"stale icon"},
            {"extension/media/gitpane.png": None},
            {"extension/bin/gitpane": b"stale backend"},
            {"extension/readme.md": b"stale documentation"},
            {"extension/src/view.js": None},
            {"extension/src/unexpected.js": b"unexpected"},
            {"extension/package.json": b"{}"},
        ]:
            with self.subTest(changes=changes):
                self.package(changes)
                with self.assertRaisesRegex(ValueError, "match|files"):
                    verify_payload(
                        self.archive, self.source, self.backend, "darwin-arm64"
                    )

    def test_wrong_platform_and_missing_executable_permission_are_rejected(
        self,
    ) -> None:
        for target, mode, message in [
            ("linux-arm64", 0o100755, "platform"),
            ("darwin-arm64", 0o100644, "executable"),
        ]:
            with self.subTest(target=target, mode=mode):
                self.package(target=target, mode=mode)
                with self.assertRaisesRegex(ValueError, message):
                    verify_payload(
                        self.archive, self.source, self.backend, "darwin-arm64"
                    )

    def release_packages(self) -> Path:
        directory = self.root / "release"
        directory.mkdir()
        for target in TARGETS:
            self.archive = directory / f"gitpane-explorer-{target}.vsix"
            self.package(target=target)
        return directory

    def test_icon_dimension_boundaries_are_verified_in_the_payload(
        self,
    ) -> None:
        icon = self.source / "media/gitpane.png"
        for width, height in [(128, 128), (256, 256), (1024, 1024)]:
            with self.subTest(width=width, height=height):
                icon.write_bytes(png_image(width, height))
                self.package()
                self.assertEqual(
                    verify_payload(
                        self.archive, self.source, None, "darwin-arm64"
                    ),
                    b"built backend",
                )
        for width, height in [(127, 256), (256, 127), (1025, 256),
                              (256, 1025), (1254, 1254)]:
            with self.subTest(width=width, height=height):
                icon.write_bytes(png_image(width, height))
                self.package()
                with self.assertRaisesRegex(ValueError, "icon dimensions"):
                    verify_payload(
                        self.archive, self.source, None, "darwin-arm64"
                    )

    def test_oversized_icons_stop_before_the_publisher_runs(self) -> None:
        (self.source / "media/gitpane.png").write_bytes(png_image(1254, 1254))
        directory = self.release_packages()
        with patch("subprocess.run") as publish:
            with self.assertRaisesRegex(ValueError, "icon dimensions"):
                publish_packages(directory, self.source, verify_only=False)
            publish.assert_not_called()

    def test_invalid_png_headers_and_missing_icon_declarations_are_rejected(
        self,
    ) -> None:
        icon = self.source / "media/gitpane.png"
        for image in [b"not a PNG", png_image(256, 256)[:24]]:
            with self.subTest(image=image):
                icon.write_bytes(image)
                self.package()
                with self.assertRaisesRegex(ValueError, "PNG IHDR"):
                    verify_payload(
                        self.archive, self.source, None, "darwin-arm64"
                    )
        icon.write_bytes(png_image(256, 256))
        self.package()
        with zipfile.ZipFile(self.archive) as package:
            metadata = package.read("extension.vsixmanifest")
        self.package({"extension.vsixmanifest": metadata.replace(
            b'Path="extension/media/gitpane.png"',
            b'Path="extension/media/missing.png"')})
        with self.assertRaisesRegex(ValueError, "icon asset"):
            verify_payload(self.archive, self.source, None, "darwin-arm64")
        manifest = json.loads((self.source / "package.json").read_text())
        manifest["icon"] = "media/missing.png"
        (self.source / "package.json").write_text(json.dumps(manifest))
        self.package()
        with self.assertRaisesRegex(ValueError, "icon must be included"):
            verify_payload(self.archive, self.source, None, "darwin-arm64")

    def test_complete_release_publishes_all_verified_targets_with_oidc(self) -> None:
        directory = self.release_packages()
        with patch("subprocess.run") as publish:
            publish_packages(directory, self.source, verify_only=True)
            publish.assert_not_called()
            publish_packages(directory, self.source, verify_only=False)
        self.assertEqual(
            publish.call_args.args[0],
            ["npx", "--yes", "@vscode/vsce@4.1.0", "publish", "--oidc",
             "--packagePath", *(str(directory / f"gitpane-explorer-{target}.vsix")
                                for target in TARGETS)],
        )

    def test_incomplete_or_mixed_release_never_starts_publication(self) -> None:
        directory = self.release_packages()
        for changes in [
            {"extension/package.json": b'{"version":"0.0.2"}'},
            {"extension/src/view.js": b"stale sources"},
            {"extension/bin/gitpane.exe": b""},
        ]:
            with self.subTest(changes=changes):
                self.package(changes, target=TARGETS[-1])
                with patch("subprocess.run") as publish:
                    with self.assertRaises(ValueError):
                        publish_packages(directory, self.source, verify_only=False)
                    publish.assert_not_called()
        self.archive.unlink()
        with patch("subprocess.run") as publish:
            with self.assertRaisesRegex(ValueError, "five platform"):
                publish_packages(directory, self.source, verify_only=False)
            publish.assert_not_called()

    def test_failed_publication_reports_possible_public_targets(self) -> None:
        directory = self.release_packages()
        with patch(
            "subprocess.run", side_effect=subprocess.CalledProcessError(1, "vsce")
        ):
            with self.assertRaisesRegex(ValueError, "may already be public"):
                publish_packages(directory, self.source, verify_only=False)

    def test_release_preflight_accepts_text_checkout_line_endings(self) -> None:
        files = {
            "extension/readme.md": self.source / "README.md",
            "extension/src/view.js": self.source / "src/view.js",
            "extension/LICENSE.txt": self.root / "LICENSE",
        }
        original = {name: file.read_bytes() for name, file in files.items()}
        for source_ending, package_ending in [
            (b"\n", b"\r\n"), (b"\r\n", b"\n")
        ]:
            with self.subTest(source_ending=source_ending):
                for name, file in files.items():
                    file.write_bytes(original[name].replace(b"\n", source_ending))
                self.package(
                    {name: data.replace(b"\n", package_ending)
                     for name, data in original.items()},
                    target="win32-x64",
                )
                self.assertEqual(
                    verify_payload(self.archive, self.source, None, "win32-x64"),
                    b"built backend",
                )
                with self.assertRaisesRegex(ValueError, "match source"):
                    verify_payload(
                        self.archive, self.source, self.backend, "win32-x64"
                    )

    def test_binary_assets_and_native_backends_keep_exact_bytes(self) -> None:
        asset = self.source / "src/asset.bin"
        asset.write_bytes(b"binary\nasset")
        self.package({"extension/src/asset.bin": b"binary\r\nasset"})
        with self.assertRaisesRegex(ValueError, "match source"):
            verify_payload(self.archive, self.source, None, "darwin-arm64")
        self.backend.write_bytes(b"binary\nbackend")
        self.package({"extension/src/asset.bin": b"binary\nasset",
                      "extension/bin/gitpane": b"binary\r\nbackend"})
        with self.assertRaisesRegex(ValueError, "match source"):
            verify_payload(
                self.archive, self.source, self.backend, "darwin-arm64"
            )


if __name__ == "__main__":
    unittest.main()
