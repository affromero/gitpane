mod action;
mod app;
mod components;
mod config;
mod event;
mod git;
mod repo_id;
mod theme;
mod tui;
mod update_checker;
mod watcher;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "gitpane", about = "Multi-repo Git workspace dashboard")]
struct Cli {
    /// Root directory to scan for repos
    #[arg(long)]
    root: Option<PathBuf>,

    /// Override the active theme for this run (does not modify config.toml)
    #[arg(long)]
    theme: Option<String>,

    /// UI frame rate (deprecated — rendering is now on-demand)
    #[arg(long, default_value_t = 10, hide = true)]
    frame_rate: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Update gitpane to the latest version via cargo install
    Update,
    /// List available themes (built-in + custom)
    Themes,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Update) => return self_update(),
        Some(Command::Themes) => return list_themes(cli.theme.as_deref()),
        None => {}
    }

    install_tracing()?;

    let mut config = config::Config::load()?;

    if let Some(root) = cli.root {
        config.override_root(root);
    }
    if let Some(theme_name) = cli.theme {
        // Apply as a session-only override so `config.save()` (triggered by
        // unrelated TUI actions: add repo, rescan, ...) does not persist
        // the CLI choice.
        config.runtime_theme_override = Some(theme_name);
        config.resolve_theme_with_env(&config::RealEnv);
    }
    config.ui.frame_rate = cli.frame_rate;

    let mut app = app::App::new(config);
    app.run().await?;

    Ok(())
}

/// Set up tracing. By default, logs go to stderr (where they get clobbered
/// by the TUI alt-screen as soon as the app starts). Set `GITPANE_LOG_FILE=...`
/// to redirect everything to a file instead — handy for capturing
/// `tracing::error!` toasts and `tracing::warn!` watcher messages while the
/// TUI is running. `GITPANE_LOG=...` is a convenience knob for the level
/// filter; equivalent to `RUST_LOG=...` but scoped so callers do not have
/// to remember to namespace it.
fn install_tracing() -> Result<()> {
    let env_var = std::env::var("GITPANE_LOG").or_else(|_| std::env::var("RUST_LOG"));
    let filter = match env_var {
        Ok(v) => tracing_subscriber::EnvFilter::new(v),
        Err(_) => tracing_subscriber::EnvFilter::new("gitpane=info"),
    };
    if let Ok(path) = std::env::var("GITPANE_LOG_FILE") {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
    Ok(())
}

fn list_themes(cli_override: Option<&str>) -> Result<()> {
    let env = config::RealEnv;
    let mut config = config::Config::load()?;
    if let Some(name) = cli_override {
        config.runtime_theme_override = Some(name.to_string());
    }
    // Use the loaded config's full search list so $GITPANE_CONFIG-adjacent
    // custom themes show up even though they live outside XDG.
    let dirs = config.theme_dirs(&env);
    let current = config.effective_theme_name();
    for name in theme::discover_all_theme_names(&dirs) {
        let marker = if name == current { "*" } else { " " };
        println!("{marker} {name}");
    }
    Ok(())
}

fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("gitpane v{current} — checking for updates...");

    if let Some(latest) = update_checker::check_latest() {
        println!("New version available: v{latest}");
    } else {
        println!("Already up to date.");
        return Ok(());
    }

    println!("Running: cargo install gitpane");
    let status = std::process::Command::new("cargo")
        .args(["install", "gitpane"])
        .status();

    match status {
        Ok(s) if s.success() => println!("Updated successfully."),
        Ok(s) => {
            eprintln!("cargo install exited with {s}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run cargo: {e}");
            eprintln!("Make sure cargo is installed (https://rustup.rs)");
            std::process::exit(1);
        }
    }

    Ok(())
}
