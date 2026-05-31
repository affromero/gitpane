run:
    cargo run

test:
    cargo test --all-targets --all-features

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets --all-features -- -D warnings

audit:
    cargo audit --deny warnings

coverage:
    cargo llvm-cov --workspace --all-targets --all-features --lcov --output-path lcov.info

ci: fmt lint test

# Recording recipes (require vhs: https://github.com/charmbracelet/vhs)

record: record-demo record-screenshots

record-demo:
    @which vhs >/dev/null 2>&1 || { echo "error: vhs not found — install from https://github.com/charmbracelet/vhs"; exit 1; }
    cargo build --release
    vhs assets/demo.tape

record-screenshots:
    @which vhs >/dev/null 2>&1 || { echo "error: vhs not found — install from https://github.com/charmbracelet/vhs"; exit 1; }
    cargo build --release
    vhs assets/screenshot-main.tape
    vhs assets/screenshot-diff.tape
    vhs assets/screenshot-commit.tape

# Remote test loop. Override HOST and REMOTE on the command line:
#   just HOST=gcloud-h100 deploy-remote
# Defaults assume an ssh alias `gcloud-h100` and an ~/gitpane-dev checkout.
HOST := "gcloud-h100"
REMOTE := "~/gitpane-dev"

# rsync source to the remote, build, and overwrite ~/.cargo/bin/gitpane.
# Requires cargo + rustc already installed on the remote.
deploy-remote:
    rsync -az --delete \
        --exclude=target --exclude=.git --exclude='*.log' \
        ./ {{HOST}}:{{REMOTE}}/
    ssh {{HOST}} 'source ~/.cargo/env && cd {{REMOTE}} && cargo build --release && cp target/release/gitpane ~/.cargo/bin/gitpane'

# Quick remote run: launches gitpane in a detached tmux pane on the remote
# with GITPANE_LOG_FILE set, sleeps SECS seconds, sends `q`, and prints the log.
# Use this when you want to capture any tracing::warn!/error! that fires
# during a short headless run.
#   just run-remote SECS=20
SECS := "10"
run-remote: deploy-remote
    ssh {{HOST}} 'rm -f /tmp/gitpane.log; tmux kill-session -t gp-test 2>/dev/null; \
        tmux new-session -d -s gp-test -x 200 -y 50 \
            -e GITPANE_LOG=gitpane=debug \
            -e GITPANE_LOG_FILE=/tmp/gitpane.log \
            ~/.cargo/bin/gitpane; \
        sleep {{SECS}}; tmux send-keys -t gp-test q; sleep 2; \
        tmux kill-session -t gp-test 2>/dev/null; \
        echo "--- /tmp/gitpane.log ---"; cat /tmp/gitpane.log'
