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

/// The current gitpane version.
///
/// Uses `GITPANE_BUILD_OVERWRITE_VERSION` when set at build time,
/// otherwise falls back to `CARGO_PKG_VERSION`.
pub const VERSION: &str = match option_env!("GITPANE_BUILD_OVERWRITE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
