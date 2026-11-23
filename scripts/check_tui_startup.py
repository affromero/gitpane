"""Exercise the built dashboard in isolated terminal environments on Unix."""

import argparse
import errno
import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path


def run_dashboard(binary: Path, directory: Path, env: dict[str, str]) -> tuple[int, bytes]:
    pid, terminal = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
            os.chdir(directory)
            os.execve(binary, [str(binary), "--cwd", "--no-sync-repos"], env)
        except OSError as error:
            os.write(2, str(error).encode())
            os._exit(125)

    deadline = time.monotonic() + 20
    output = bytearray()
    sent_quit = False
    terminal_open = True
    reaped = False
    try:
        while (remaining := deadline - time.monotonic()) > 0:
            readers = [terminal] if terminal_open else []
            ready, _, _ = select.select(readers, [], [], min(remaining, 0.1))
            if ready:
                try:
                    data = os.read(terminal, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""
                terminal_open = bool(data)
                output.extend(data)
                if not sent_quit and b"Repositories" in output:
                    os.write(terminal, b"q")
                    sent_quit = True
            if not terminal_open:
                exited, status = os.waitpid(pid, os.WNOHANG)
                if exited:
                    reaped = True
                    return os.waitstatus_to_exitcode(status), bytes(output)
        raise AssertionError(f"dashboard did not exit within 20 seconds: {bytes(output)!r}")
    finally:
        os.close(terminal)
        if not reaped:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)


def check_startup(binary: Path) -> None:
    git = shutil.which("git")
    if git is None:
        raise RuntimeError("git is required to set up the TUI startup fixture")
    with tempfile.TemporaryDirectory(prefix="gitpane-startup-") as temp:
        root = Path(temp)
        for name in ("home", "bin", "repo"):
            (root / name).mkdir()
        (root / "bin" / "git").symlink_to(git)
        env = {
            "HOME": str(root / "home"),
            "XDG_CONFIG_HOME": str(root / "home" / "config"),
            "XDG_DATA_HOME": str(root / "home" / "data"),
            "PATH": str(root / "bin"),
            "TERM": "xterm-256color",
            "LC_ALL": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "HC_TERMINAL_ID": "t1",
        }
        repo = root / "repo"
        subprocess.run([git, "init", "--quiet"], cwd=repo, env=env, check=True)
        desktop = root / "bin" / "herdr-app"
        for name, diagnostic, expected_status, expected_output in (
            ("missing CLI", None, 0, b"Repositories"),
            (
                "missing socket",
                "herdr-app: no Herdr window at /test/herdr-app.sock: No such file or directory (os error 2)",
                0,
                b"Repositories",
            ),
            (
                "probe failure",
                "herdr-app: layout request failed",
                1,
                b"Could not detect Herdr desktop session: herdr-app ls failed: herdr-app: layout request failed",
            ),
        ):
            if diagnostic is not None:
                desktop.write_text(f"#!/bin/sh\nprintf '%s\\n' '{diagnostic}' >&2\nexit 2\n")
                desktop.chmod(0o700)
            status, output = run_dashboard(binary, repo, env)
            if status != expected_status or expected_output not in output:
                raise AssertionError(
                    f"{name}: expected status {expected_status} and {expected_output!r}, "
                    f"got {status} and {output!r}"
                )
            print(f"TUI startup passed: {name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    check_startup(parser.parse_args().binary.resolve(strict=True))
