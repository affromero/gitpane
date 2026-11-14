"""Record the TUI demo using VHS and a disposable sample workspace.

Run `cargo build --bin gitpane`, then `python3 assets/demo-tui.py`.
Set GITPANE_DEMO_BINARY when the built binary lives outside target/debug.
"""

import os
from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("GITPANE_DEMO_BINARY", root / "target/debug/gitpane"))
if not binary.is_file():
    raise SystemExit(f"Build gitpane first: {binary} does not exist")

with tempfile.TemporaryDirectory(prefix="gitpane-demo-") as directory:
    demo = Path(directory)
    subprocess.run(["sh", str(root / "assets/demo-workspace.sh"), str(demo)], check=True)
    environment = dict(os.environ, GITPANE_DEMO_ROOT=str(demo / "workspace"),
                       GITPANE_DEMO_CONFIG=str(demo / "config/config.toml"),
                       GITPANE_DEMO_BINARY=str(binary.resolve()),
                       GITPANE_DEMO_SOCKET=demo.name)
    subprocess.run(["vhs", "assets/demo.tape"], cwd=root, env=environment, check=True)
