"""Exercise source-package testing with Cargo as the process boundary."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check_source_package.py").resolve()


@unittest.skipUnless(os.name == "posix", "executable Cargo stand-in requires Unix")
class SourcePackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.checkout = self.root / "checkout"
        self.checkout.mkdir()
        (self.checkout / "Cargo.toml").write_text("checkout only")
        (self.checkout / "scripts").mkdir()
        shutil.copy(SCRIPT.with_name("check_test_environment.py"),
                    self.checkout / "scripts/check_test_environment.py")
        self.observed = self.root / "observed.json"
        for name in ("sh", "sleep", "git"):
            (self.bin / name).symlink_to(shutil.which(name))

    def cargo(self, fail_stage="", exit_code=0):
        source = f'''#!{sys.executable}
import io, json, os, pathlib, sys, tarfile
stage = sys.argv[1]
if stage == "test" and "--no-run" in sys.argv:
    stage = "restricted"
if stage == {fail_stage!r}:
    print("deliberate Cargo failure", file=sys.stderr)
    raise SystemExit({exit_code})
root = pathlib.Path.cwd()
observed = pathlib.Path({str(self.observed)!r})
if stage == "metadata":
    print(json.dumps({{"packages": [{{"name": "gitpane", "version": "1.2.3",
          "manifest_path": str(root / "Cargo.toml")}}]}}))
elif stage == "package":
    target = pathlib.Path(os.environ["CARGO_TARGET_DIR"])
    (target / "package").mkdir(parents=True)
    with tarfile.open(target / "package/gitpane-1.2.3.crate", "w:gz") as archive:
        data = b"packaged source"
        info = tarfile.TarInfo("gitpane-1.2.3/Cargo.toml")
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
elif stage == "test":
    assert (root / "Cargo.toml").read_text() == "packaged source"
    assert not (root / ".git").exists()
    observed.write_text(json.dumps({{"source": str(root),
        "target": os.environ["CARGO_TARGET_DIR"], "normal": True}}))
elif stage == "restricted":
    harness = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "package-test"
    harness.write_text({('#!' + sys.executable + chr(10))!r} +
        "import json, pathlib, shutil\\n" +
        "p = pathlib.Path(" + repr(str(observed)) + ")\\n" +
        "data = json.loads(p.read_text())\\n" +
        "data['restricted'] = shutil.which('git') is None\\n" +
        "p.write_text(json.dumps(data))\\n")
    harness.chmod(0o755)
    print(json.dumps({{"reason": "compiler-artifact", "profile": {{"test": True}},
        "target": {{"name": "gitpane", "kind": ["bin"]}},
        "executable": str(harness)}}))
'''
        executable = self.bin / "cargo"
        executable.write_text(source)
        executable.chmod(0o755)

    def run_check(self):
        return subprocess.run(
            [sys.executable, str(SCRIPT)], cwd=self.checkout,
            env=dict(os.environ, PATH=str(self.bin),
                     CARGO_TARGET_DIR=str(self.checkout / "target")),
            capture_output=True, text=True, check=False,
        )

    def test_runs_packaged_source_with_and_without_git_in_fresh_target(self):
        self.cargo()
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        observed = json.loads(self.observed.read_text())
        self.assertTrue(observed["normal"])
        self.assertTrue(observed["restricted"])
        self.assertFalse(Path(observed["source"]).is_relative_to(self.checkout))
        self.assertFalse(Path(observed["target"]).is_relative_to(self.checkout))
        self.assertFalse((self.checkout / "target").exists())

    def test_package_and_test_failures_stop_validation(self):
        for stage in ("metadata", "package", "test", "restricted"):
            with self.subTest(stage=stage):
                self.observed.unlink(missing_ok=True)
                self.cargo(fail_stage=stage, exit_code=37)
                result = self.run_check()
                self.assertEqual(result.returncode, 37, result.stdout + result.stderr)
                self.assertIn("deliberate Cargo failure", result.stderr)
                if self.observed.exists():
                    self.assertNotIn("restricted", json.loads(self.observed.read_text()))


if __name__ == "__main__":
    unittest.main()
