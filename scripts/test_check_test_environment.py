"""Exercise the restricted-PATH runner using stand-ins at process boundaries."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check_test_environment.py")


@unittest.skipUnless(os.name == "posix", "restricted PATH check requires Unix")
class TestEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("sh", "sleep"):
            (self.bin / name).symlink_to(shutil.which(name))
        self.executable("bin/git", "raise SystemExit(0)")
        self.observed = self.root / "observed.json"

    def executable(self, name, source):
        path = self.root / name
        path.write_text(f"#!{sys.executable}\n{source}\n", encoding="utf-8")
        path.chmod(0o755)
        return path

    def cargo(self, kinds=("lib", "bin"), test_exit=0, build_exit=0):
        artifacts = []
        for kind in kinds:
            test = self.executable(
                f"test-{kind}",
                "import json, shutil\n"
                "from pathlib import Path\n"
                f"observed = Path({str(self.observed)!r})\n"
                "data = json.loads(observed.read_text()) if observed.exists() else {}\n"
                f"data[{kind!r}] = {{name: shutil.which(name) "
                "for name in ('git', 'sh', 'sleep', 'cargo')}\n"
                "observed.write_text(json.dumps(data))\n"
                f"raise SystemExit({test_exit})",
            )
            artifacts.append({
                "reason": "compiler-artifact", "profile": {"test": True},
                "target": {"name": "gitpane", "kind": [kind]},
                "executable": str(test),
            })
        messages = "\n".join(json.dumps(item) for item in artifacts)
        self.executable(
            "bin/cargo", f"print({messages!r})\nraise SystemExit({build_exit})"
        )

    def run_check(self):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            env=dict(os.environ, PATH=str(self.bin)),
            capture_output=True, text=True, check=False,
        )

    def test_runs_binary_and_library_with_git_removed_from_child_path(self):
        self.cargo()
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        observed = json.loads(self.observed.read_text())
        self.assertEqual(set(observed), {"bin", "lib"})
        for commands in observed.values():
            self.assertIsNone(commands["git"])
            self.assertIsNone(commands["cargo"])
            self.assertIsNotNone(commands["sh"])
            self.assertIsNotNone(commands["sleep"])
        self.assertIsNotNone(shutil.which("git", path=str(self.bin)))

    def test_rejects_builds_without_the_binary_test_harness(self):
        for kinds in ((), ("lib",)):
            with self.subTest(kinds=kinds):
                self.cargo(kinds=kinds)
                result = self.run_check()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("binary test harness", result.stderr)
                self.assertFalse(self.observed.exists())

    def test_propagates_test_failure(self):
        self.cargo(kinds=("bin",), test_exit=37)
        result = self.run_check()
        self.assertEqual(result.returncode, 37, result.stdout + result.stderr)
        self.assertTrue(self.observed.exists())

    def test_does_not_run_tests_after_build_failure(self):
        self.cargo(build_exit=23)
        result = self.run_check()
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(self.observed.exists())


if __name__ == "__main__":
    unittest.main()
