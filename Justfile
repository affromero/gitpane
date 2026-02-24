run:
    cargo run

test:
    cargo test

fmt:
    cargo fmt --all

lint:
    cargo clippy --all-targets --all-features

ci: fmt lint test
