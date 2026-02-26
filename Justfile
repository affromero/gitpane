run:
    cargo run

test:
    cargo test

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets --all-features

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
