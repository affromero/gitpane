mod action;
mod app;
mod components;
mod config;
mod diagnostic;
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
#[command(name = "gitpane", about = "Multi-repo Git workspace dashboard", version = gitpane::VERSION)]
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
    /// Print configuration and workspace diagnostics
    #[command(alias = "diagnostics")]
    Diagnostic,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Update) => return self_update(),
        Some(Command::Themes) => return list_themes(cli.theme.as_deref()),
        Some(Command::Diagnostic) => return run_diagnostic(cli.root, cli.theme.as_deref()),
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

/// Set up tracing. By default, tracing is disabled so log lines cannot corrupt
/// the alternate-screen TUI. Set `GITPANE_LOG_FILE=...` to capture logs in a
/// file, and set `GITPANE_LOG=...` or `RUST_LOG=...` to opt into stderr logging
/// for foreground debugging.
fn install_tracing() -> Result<()> {
    let env_filter = std::env::var("GITPANE_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok();
    let log_file = std::env::var("GITPANE_LOG_FILE").ok();

    if env_filter.is_none() && log_file.is_none() {
        return Ok(());
    }

    let filter = match env_filter {
        Some(v) => tracing_subscriber::EnvFilter::new(v),
        None => tracing_subscriber::EnvFilter::new("gitpane=info"),
    };
    if let Some(path) = log_file {
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

fn run_diagnostic(root: Option<PathBuf>, theme_override: Option<&str>) -> Result<()> {
    let env = config::RealEnv;
    let mut config = config::Config::load()?;
    if let Some(root) = root {
        config.override_root(root);
    }
    if let Some(name) = theme_override {
        config.runtime_theme_override = Some(name.to_string());
        config.resolve_theme_with_env(&env);
    }
    let repos = git::scanner::discover_repos(&config);
    let report = diagnostic::render(
        &config,
        &repos,
        diagnostic::RuntimeInfo::current(gitpane::VERSION),
    );
    print!("{report}");
    Ok(())
}

fn self_update() -> Result<()> {
    let base = env!("CARGO_PKG_VERSION");
    println!("gitpane v{} — checking for updates...", gitpane::VERSION);
    if gitpane::VERSION != base {
        println!(
            "note: this build sets a custom version; update checks use the base version {base}"
        );
        println!(
            "note: `gitpane update` runs `cargo install gitpane` and may replace a package-managed binary"
        );
    }

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
