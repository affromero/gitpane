//! # gitpane
//!
//! `gitpane` is a multi repo Git workspace dashboard for the terminal.
//!
//! The package is distributed primarily as a command line application. This
//! small library target gives docs.rs and Rust documentation tooling a stable
//! package page.
//!
//! Install and run:
//!
//! ```text
//! cargo install gitpane
//! gitpane
//! gitpane diagnostic
//! ```
//!
//! The public library surface is intentionally small.

/// The crate version from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
