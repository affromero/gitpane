"""Behavioral checks for the release gate, using temporary release files."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_metadata.py")


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.write_versions("1.2.3", "1.2.3")
        self.changelog = self.root / "CHANGELOG.md"
        self.changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\nFuture work.\n\n"
            "## [1.2.3] - 2026-09-08\n\n### Fixed\n\n- Keep Unicode café readable.\n\n"
            "## [1.2.2]\n\nOlder work.\n", encoding="utf-8"
        )
        self.notes = self.root / "release-notes.md"

    def write_versions(self, manifest, locked):
        (self.root / "Cargo.toml").write_text(
            f'[package]\nname = "gitpane"\nversion = "{manifest}"\n', encoding="utf-8"
        )
        (self.root / "Cargo.lock").write_text(
            f'version = 4\n[[package]]\nname = "gitpane"\nversion = "{locked}"\n',
            encoding="utf-8",
        )

    def validate(self, tag="v1.2.3"):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(self.root),
             "--tag", tag, "--notes", str(self.notes)],
            capture_output=True, text=True, check=False,
        )

    def assert_rejected(self, result, expected):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(expected, result.stderr)
        self.assertFalse(self.notes.exists(), "invalid metadata produced release notes")

    def test_valid_release_extracts_only_its_own_section(self):
        result = self.validate()
        self.assertEqual(result.returncode, 0, result.stderr)
        notes = self.notes.read_text(encoding="utf-8")
        self.assertIn("## [1.2.3] - 2026-09-08", notes)
        self.assertIn("café", notes)
        self.assertNotIn("Future work", notes)
        self.assertNotIn("Older work", notes)

    def test_manifest_and_lock_versions_must_both_match_the_tag(self):
        for manifest, locked in [("1.2.2", "1.2.3"), ("1.2.3", "1.2.2")]:
            with self.subTest(manifest=manifest, locked=locked):
                self.write_versions(manifest, locked)
                self.assert_rejected(self.validate(), "version 1.2.3")

    def test_only_stable_semantic_version_tags_are_accepted(self):
        for tag in ["1.2.3", "v1x2x3", "v1.2", "v01.2.3", "v1.2.3-rc.1"]:
            with self.subTest(tag=tag):
                self.assert_rejected(self.validate(tag), "vX.Y.Z")

    def test_changelog_version_dots_are_literal(self):
        self.changelog.write_text("## [1x2x3]\n\nWrong release.\n", encoding="utf-8")
        self.assert_rejected(self.validate(), "## [1.2.3]")

    def test_missing_duplicate_and_empty_release_sections_are_rejected(self):
        for changelog in [
            "## [Unreleased]\n\nPending.\n",
            "## [1.2.3]\n\nFirst.\n\n## [1.2.3]\n\nDuplicate.\n",
            "## [1.2.3]\n\n\t\n## [1.2.2]\n\nOlder.\n",
            "## [1.2.3]\n\n### Added\n\n### Fixed\n",
        ]:
            with self.subTest(changelog=changelog):
                self.changelog.write_text(changelog, encoding="utf-8")
                self.assert_rejected(self.validate(), "CHANGELOG.md")

    def test_plain_paragraph_notes_without_categories_are_valid(self):
        self.changelog.write_text("## [1.2.3]\n\nFix startup.\n", encoding="utf-8")
        result = self.validate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Fix startup.", self.notes.read_text(encoding="utf-8"))

    def test_missing_or_duplicate_lock_packages_are_rejected(self):
        package = '[[package]]\nname = "gitpane"\nversion = "1.2.3"\n'
        for contents in ["version = 4\npackage = []\n", "version = 4\n" + package * 2]:
            with self.subTest(contents=contents):
                (self.root / "Cargo.lock").write_text(contents, encoding="utf-8")
                self.assert_rejected(self.validate(), "Cargo.lock")

    def test_malformed_manifest_fails_without_writing_notes(self):
        (self.root / "Cargo.toml").write_text("[invalid", encoding="utf-8")
        self.assert_rejected(self.validate(), "Release validation failed")


if __name__ == "__main__":
    unittest.main()
