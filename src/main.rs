mod action;
mod app;
mod components;
mod config;
mod event;
mod git;
mod repo_id;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    if let Some(Command::Update) = cli.command {
        return self_update();
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
    config.ui.frame_rate = cli.frame_rate;

    let mut app = app::App::new(config);
    app.run().await?;

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
