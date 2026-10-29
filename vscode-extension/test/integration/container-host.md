# Isolated Linux VS Code host tests

The optional container runner starts a Linux VS Code test host inside a
disposable container. Run it only when launching a VS Code test host is
authorized. It does not use the host's VS Code profile or display.

Stage an immutable Linux image locally with `node`, `git`, `code`,
`xvfb-run`, and `/usr/bin/timeout` on its PATH. The image must provide a
usable default home for the non-root test user and must work with a read-only
root filesystem. Extract a Linux-target Gitpane VSIX to a directory on the
host, then run:

```sh
node vscode-extension/test/integration/container-host.js \
  --image sha256:<local-image-id> --extension /absolute/path/to/extension
```

Set `GITPANE_CONTAINER_SUITE=scale` to select the scale suite. The runner
copies its test scripts and the extracted extension into private staging
directories. It starts the pre-staged image without network access and checks
the container's identity and isolation before cleanup. The writable results
directory is monitored for size, but this polling is not a hard disk quota and
can overshoot. Logs are bounded and explicitly marked if truncated.

The runner prints its artifact directory. Failed or unverified runs retain
their artifacts. If the Docker daemon cannot be reached during cleanup, the
reported run label identifies the potentially remaining test container; do
not assume it was stopped. A Linux container host test does not validate a
native macOS or Windows extension host, Remote SSH, or WSL.
