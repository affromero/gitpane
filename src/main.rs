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
        Some(Command::Themes) => return list_themes(),
        None => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("gitpane=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut config = config::Config::load()?;

    if let Some(root) = cli.root {
        config.override_root(root);
    }
    if let Some(theme_name) = cli.theme {
        config.theme_name = theme_name;
        config.resolve_theme_with_env(&config::RealEnv);
    }
    config.ui.frame_rate = cli.frame_rate;

    let mut app = app::App::new(config);
    app.run().await?;

    Ok(())
}

fn list_themes() -> Result<()> {
    let env = config::RealEnv;
    let config = config::Config::load()?;
    // Use the loaded config's full search list so $GITPANE_CONFIG-adjacent
    // custom themes show up even though they live outside XDG.
    let dirs = config.theme_dirs(&env);
    for name in theme::discover_all_theme_names(&dirs) {
        let marker = if name == config.theme_name { "*" } else { " " };
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
