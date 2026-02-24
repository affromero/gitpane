mod action;
mod app;
mod components;
mod config;
mod event;
mod git;
mod tui;
mod watcher;

use clap::Parser;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("gitpane=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let mut config = config::Config::load()?;

    if let Some(root) = cli.root {
        config.override_root(root);
    }
    config.ui.frame_rate = cli.frame_rate;

    let mut app = app::App::new(config);
    app.run().await?;

    Ok(())
}
