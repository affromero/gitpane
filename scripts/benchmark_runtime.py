"""Compare GitPane TUI binaries on the same disposable workspace."""

import argparse
import errno
import fcntl
import hashlib
import json
import os
import platform
import pty
import re
import resource
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
from collections.abc import Callable
from pathlib import Path


TIMEOUT = 30
READY = re.compile(r"filesystem watcher ready: (\d+) repos in (\d+(?:\.\d+)?)(ns|µs|ms|s)")
WATCH_FAILURE = re.compile(
    r"Failed to (?:watch root|watch repo|rebuild filesystem watcher)|skip watch on|"
    r"Could not (?:read|raise) the open-file limit"
)
MARKER = b"bench-edit.txt"
METRICS = {
    "first_frame_ms": "First rendered frame (ms)",
    "watcher_setup_ms": "Watcher setup (ms)",
    "edit_visible_ms": "File creation to visible change (ms)",
    "idle_cpu_percent": "Idle CPU (% of one core)",
    "idle_rss_mib": "Idle resident memory (MiB)",
    "peak_rss_mib": "Peak resident memory (MiB)",
    "process_cpu_ms": "Total process CPU (ms)",
}


class Terminal:
    def __init__(
        self, binary: Path, workspace: Path, env: dict[str, str], open_files: int
    ) -> None:
        self.output = bytearray()
        self.reaped = False
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 160, 0, 0))
                if open_files:
                    _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
                    resource.setrlimit(resource.RLIMIT_NOFILE, (open_files, hard))
                os.chdir(workspace)
                os.execve(binary, [str(binary), "--root", str(workspace), "--no-sync-repos"], env)
            except (OSError, ValueError) as error:
                os.write(2, str(error).encode())
                os._exit(125)

    def read(self, seconds: float) -> bool:
        if not select.select([self.fd], [], [], seconds)[0]:
            return True
        try:
            data = os.read(self.fd, 65_536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            data = b""
        self.output.extend(data)
        del self.output[:-262_144]
        return bool(data)

    def wait_for(self, ready: Callable[[], bool], description: str) -> None:
        deadline = time.monotonic() + TIMEOUT
        while not ready():
            if time.monotonic() >= deadline or not self.read(0.02):
                raise RuntimeError(
                    f"Could not observe {description}. Terminal tail:\n"
                    f"{bytes(self.output[-3000:])!r}"
                )

    def idle(self, seconds: float) -> None:
        deadline = time.monotonic() + seconds
        while (remaining := deadline - time.monotonic()) > 0:
            if not self.read(min(remaining, 0.1)):
                raise RuntimeError("GitPane exited during the idle sample")

    def finish(self) -> resource.struct_rusage:
        os.write(self.fd, b"q")
        deadline = time.monotonic() + TIMEOUT
        while time.monotonic() < deadline:
            pid, status, usage = os.wait4(self.pid, os.WNOHANG)
            if pid:
                self.reaped = True
                if os.waitstatus_to_exitcode(status) != 0:
                    raise RuntimeError(f"GitPane exited with status {os.waitstatus_to_exitcode(status)}")
                return usage
            if not self.read(0.02):
                time.sleep(0.02)
        raise RuntimeError("GitPane did not exit after q")

    def close(self) -> None:
        if not self.reaped:
            try:
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(self.pid, 0)
        os.close(self.fd)


def cpu_seconds(pid: int) -> float:
    if sys.platform.startswith("linux"):
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    clock = subprocess.check_output(["ps", "-p", str(pid), "-o", "time="], text=True)
    elapsed = 0.0
    for field in clock.strip().split(":"):
        elapsed = elapsed * 60 + float(field)
    return elapsed


def log_text(log: Path) -> str:
    return log.read_text(errors="replace") if log.exists() else ""


def fixture(root: Path, args: argparse.Namespace) -> tuple[Path, Path, dict[str, str]]:
    workspace = root / "workspace"
    workspace.mkdir()
    home = root / "home"
    home.mkdir()
    bin_dir = root / "bin"
    bin_dir.mkdir()
    git = shutil.which("git")
    if git is None:
        raise RuntimeError("Git is required to create the benchmark workspace")
    (bin_dir / "git").symlink_to(Path(git).resolve())
    config = root / "config.toml"
    config.write_text(
        "[ui]\ncheck_for_updates = false\nshow_liveness = false\n"
        "[github]\nenabled = false\n"
        "[watch]\nsleep_when_hidden = false\ndoze_after_secs = 0\n"
        f"watch_worktree_dirs = {str(args.nested_watches).lower()}\n"
        f"poll_local_secs = {args.poll_local_secs}\n"
        f"refresh_cooldown_ms = {args.refresh_cooldown_ms}\n"
    )
    env = {
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "PATH": os.pathsep.join((str(bin_dir), os.defpath)),
        "TERM": "xterm-256color",
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
        "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z",
        "GITPANE_CONFIG": str(config),
        "GITPANE_LOG": "gitpane=info,gitpane::watcher=debug",
    }
    for index in range(args.repos):
        repo = workspace / f"repo-{index:04}"
        repo.mkdir()
        subprocess.run([git, "init", "-q", "-b", "main"], cwd=repo, env=env, check=True)
        (repo / "README.md").write_text(f"# Benchmark repo {index}\n")
        for directory in range(args.modules_per_repo):
            module = repo / f"module-{directory:04}" / "src"
            module.mkdir(parents=True)
            (module / "data.txt").write_text("benchmark workspace\n")
        subprocess.run([git, "add", "."], cwd=repo, env=env, check=True)
        subprocess.run(
            [git, "-c", "user.name=GitPane Benchmark", "-c", "user.email=benchmark@example.invalid",
             "commit", "-qm", "Create benchmark fixture"],
            cwd=repo, env=env, check=True,
        )
    return workspace, workspace / "repo-0000" / MARKER.decode(), env


def sample(
    binary: Path, workspace: Path, marker: Path, env: dict[str, str], args: argparse.Namespace
) -> dict[str, float]:
    log = workspace.parent / "runtime.log"
    log.unlink(missing_ok=True)
    started = time.perf_counter()
    terminal = Terminal(binary, workspace, {**env, "GITPANE_LOG_FILE": str(log)}, args.open_file_limit)
    try:
        terminal.wait_for(lambda: b"Repositories" in terminal.output, "the first frame")
        first_frame_ms = (time.perf_counter() - started) * 1000
        terminal.wait_for(lambda: bool(READY.search(log_text(log))), "watcher readiness")
        match = READY.search(log_text(log))
        if match is None or int(match[1]) != args.repos:
            raise RuntimeError(f"Watcher did not report the expected {args.repos} repositories")
        setup_ms = float(match[2]) * {"ns": 0.000001, "µs": 0.001, "ms": 1, "s": 1000}[match[3]]
        terminal.wait_for(lambda: b"No changes" in terminal.output, "the clean Changes pane")
        # Measure edits at the same offset from launch so a faster watcher does
        # not simply put the edit farther from the next status poll.
        settle_remaining = args.settle_seconds - (time.perf_counter() - started)
        if settle_remaining <= 0:
            raise RuntimeError("Startup exceeded the settle window. Increase --settle-seconds for both builds.")
        terminal.idle(settle_remaining)
        cpu_start = cpu_seconds(terminal.pid)
        idle_start = time.perf_counter()
        terminal.idle(args.idle_seconds)
        idle_elapsed = time.perf_counter() - idle_start
        idle_cpu = (cpu_seconds(terminal.pid) - cpu_start) / idle_elapsed * 100
        rss = subprocess.check_output(["ps", "-p", str(terminal.pid), "-o", "rss="], text=True)
        edit_start = time.perf_counter()
        marker.write_text("working-tree change\n")
        terminal.wait_for(lambda: MARKER in terminal.output, "the edit in the Changes pane")
        edit_ms = (time.perf_counter() - edit_start) * 1000
        usage = terminal.finish()
        if WATCH_FAILURE.search(log_text(log)):
            raise RuntimeError("Watcher registration was incomplete. Check the log or increase --open-file-limit.")
        return {
            "first_frame_ms": first_frame_ms,
            "watcher_setup_ms": setup_ms,
            "edit_visible_ms": edit_ms,
            "idle_cpu_percent": idle_cpu,
            "idle_rss_mib": int(rss) / 1024,
            "peak_rss_mib": usage.ru_maxrss / (1024 ** (2 if sys.platform == "darwin" else 1)),
            "process_cpu_ms": (usage.ru_utime + usage.ru_stime) * 1000,
        }
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        raise RuntimeError(f"{binary.name}: {error}\n{log_text(log)[-6000:]}") from error
    finally:
        terminal.close()
        marker.unlink(missing_ok=True)


def positive_integer(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("expected a positive integer")
    return parsed


def binary_id(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"{path.name} ({digest.hexdigest()[:12]})"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--repos", type=positive_integer, default=20)
    parser.add_argument("--modules-per-repo", type=positive_integer, default=10)
    parser.add_argument("--runs", type=positive_integer, default=5)
    parser.add_argument("--settle-seconds", type=positive_integer, default=6)
    parser.add_argument("--idle-seconds", type=positive_integer, default=6)
    parser.add_argument("--nested-watches", action="store_true")
    parser.add_argument("--open-file-limit", type=positive_integer, default=0)
    parser.add_argument("--poll-local-secs", type=positive_integer, default=5)
    parser.add_argument("--refresh-cooldown-ms", type=int, default=5000)
    parser.add_argument("--format", choices=("markdown", "json"), default="markdown")
    args = parser.parse_args()
    if args.refresh_cooldown_ms < 0:
        parser.error("--refresh-cooldown-ms cannot be negative")
    if sys.platform != "darwin" and not sys.platform.startswith("linux"):
        parser.error("the runtime benchmark requires macOS or Linux")
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    if args.open_file_limit and hard != resource.RLIM_INFINITY and args.open_file_limit > hard:
        parser.error("--open-file-limit exceeds the inherited hard limit")
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    for path in binaries.values():
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"not an executable binary: {path}")
    samples: dict[str, list[dict[str, float]]] = {"baseline": [], "candidate": []}
    with tempfile.TemporaryDirectory(prefix="gitpane-runtime-") as temporary:
        workspace, marker, env = fixture(Path(temporary).resolve(), args)
        for run in range(args.runs + 1):
            order = tuple(binaries) if run % 2 == 0 else tuple(reversed(binaries))
            for kind in order:
                phase = "warmup" if run == 0 else f"run {run}/{args.runs}"
                print(f"{kind}: {phase}", file=sys.stderr)
                result = sample(binaries[kind], workspace, marker, env, args)
                if run:
                    samples[kind].append(result)
    medians = {
        kind: {name: statistics.median(item[name] for item in items) for name in METRICS}
        for kind, items in samples.items()
    }
    settings = {
        "host": platform.platform(),
        "repos": args.repos,
        "modules_per_repo": args.modules_per_repo,
        "nested_watches": args.nested_watches,
        "runs": args.runs,
        "idle_seconds": args.idle_seconds,
        "settle_seconds": args.settle_seconds,
        "initial_open_file_limit": args.open_file_limit or soft,
        "poll_local_secs": args.poll_local_secs,
        "refresh_cooldown_ms": args.refresh_cooldown_ms,
    }
    ids = {kind: binary_id(path) for kind, path in binaries.items()}
    if args.format == "json":
        print(json.dumps({"settings": settings, "binaries": ids, "medians": medians, "samples": samples}, indent=2))
        return
    print("### TUI runtime benchmark\n")
    print(f"{settings['host']}, {args.repos} repos, {args.modules_per_repo * 2} nested directories per repo, "
          f"nested watches {'on' if args.nested_watches else 'off'}, "
          f"initial open-file limit {settings['initial_open_file_limit']}.")
    print(f"Median of {args.runs} runs per binary after one warmup each. "
          f"Idle sampling starts {args.settle_seconds}s after launch and runs for {args.idle_seconds}s. "
          f"Status polling every {args.poll_local_secs}s, "
          f"watcher refresh cooldown {args.refresh_cooldown_ms} ms.")
    print(f"\nBaseline {ids['baseline']} · Candidate {ids['candidate']}\n")
    print("| Metric | Baseline | Candidate | Change |")
    print("| --- | ---: | ---: | ---: |")
    for name, label in METRICS.items():
        before, after = medians["baseline"][name], medians["candidate"][name]
        change = f"{(after / before - 1) * 100:+.1f}%" if before else "n/a"
        print(f"| {label} | {before:.2f} | {after:.2f} | {change} |")
    print("\nIdle CPU and RSS sample the GitPane PID. Peak RSS and total CPU come from process exit statistics.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
