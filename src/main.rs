mod action;
mod app;
mod components;
mod config;
mod diagnostic;
mod event;
mod git;
mod repo_id;
mod session;
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

    /// Scan the current directory for repos for this run, without touching
    /// the configured root_dirs. Prefer it to `--root` when you are already
    /// standing where you want to scan; explicit paths stay with `--root`.
    #[arg(long)]
    cwd: bool,

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

/// Run the app on an explicitly-owned multi-thread runtime, then drop it
/// with `shutdown_background` so quitting never waits for in-flight
/// `spawn_blocking` status queries to finish.
///
/// On a large workspace a poll can leave several libgit2 status queries
/// queued in the blocking pool; tokio's `Runtime` Drop waits forever for
/// `spawn_blocking` tasks to return, which made pressing `q` hang for tens
/// of seconds while the last poll drained. `shutdown_background` unblocks
/// shutdown immediately. That is safe for everything that can still be in
/// flight here: in-process status queries only read `.git`, and the status
/// poll's `git fetch` child (spawned with null stdio) survives as an
/// independent, crash-safe git process. Mutating operations (pull, push,
/// submodule updates) hold piped stdio that would SIGPIPE the child
/// mid-write, so the app instead gates quitting on their completion (see
/// `GitOpGuard` in `app`); they only reach this line still running when the
/// user force-quits with a second `q`.
///
/// `block_on` runs under `catch_unwind` so a panic inside the app also
/// takes the `shutdown_background` path — unwinding through the runtime's
/// normal Drop would block on the same spawn_blocking tasks and delay the
/// panic report by up to the fetch timeout.
fn main() -> Result<()> {
    color_eyre::install()?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(run())));

    // A panic skipped the app's quit gating, so a mutating git child may
    // still be running with piped stdio; give it a bounded window before
    // shutdown closes the pipes. Normal exits reach here with the counter at
    // zero (the run loop gates on it), and force-quit is the user's explicit
    // choice to skip waiting.
    if result.is_err() {
        let waited = std::time::Instant::now();
        while app::mutating_git_ops() > 0 && waited.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Do not wait for in-flight blocking-pool tasks (see doc above).
    runtime.shutdown_background();

    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Update) => return self_update(),
        Some(Command::Themes) => return list_themes(cli.theme.as_deref()),
        Some(Command::Diagnostic) => {
            return run_diagnostic(cli.root, cli.cwd, cli.theme.as_deref());
        }
        None => {}
    }
    install_tracing()?;

    let mut config = config::Config::load()?;

    if let Some(root) = cli.root {
        config.override_root(root);
    }
    // `--cwd` wins over `--root`: it is the more specific "I am here"
    // intent. Resolving to `current_dir()` keeps the override absolute, so
    // even if it ever leaked into a saved config it would not silently
    // follow wherever the next run happens to start. Both overrides are
    // run-local and never written back to config.toml.
    if cli.cwd {
        config.override_root(std::env::current_dir()?);
    }
    if let Some(theme_name) = cli.theme {
        // Apply as a session-only override so `config.save()` (triggered by
        // unrelated TUI actions: add repo, rescan, ...) does not persist
        // the CLI choice.
        config.runtime_theme_override = Some(theme_name);
        config.resolve_theme_with_env(&config::RealEnv);
    }
    config.ui.frame_rate = cli.frame_rate;

    // Under herdr, forward right-click gestures to this pane so gitpane's
    // context menu works (herdr's own right-click menu would swallow them).
    // Fire-and-forget on the blocking pool so a wedged herdr never stalls
    // startup; the helper itself no-ops outside herdr.
    tokio::task::spawn_blocking(crate::session::launcher::forward_right_click_in_herdr);

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

fn run_diagnostic(root: Option<PathBuf>, cwd: bool, theme_override: Option<&str>) -> Result<()> {
    let env = config::RealEnv;
    let mut config = config::Config::load()?;
    if let Some(root) = root {
        config.override_root(root);
    }
    if cwd {
        config.override_root(std::env::current_dir()?);
    }
    if let Some(name) = theme_override {
        config.runtime_theme_override = Some(name.to_string());
        config.resolve_theme_with_env(&env);
    }
    let repos = git::scanner::discover_repos(&config);
    let shadowed = config.shadowed_config_paths(&env);
    let report = diagnostic::render(
        &config,
        &repos,
        &shadowed,
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

    let Some(latest) = update_checker::check_latest() else {
        println!("Already up to date.");
        return Ok(());
    };
    println!("New version available: v{latest}");

    // Pin the version the checker announced. The checker reads the GitHub
    // release, which exists minutes before the crates.io publish lands; a
    // bare `cargo install gitpane` in that window quietly no-ops on the
    // already-installed version while we'd still claim success.
    println!("Running: cargo install gitpane --version {latest}");
    let status = std::process::Command::new("cargo")
        .args(["install", "gitpane", "--version", &latest])
        .status();

    match status {
        Ok(s) if s.success() => println!("Updated to v{latest}."),
        Ok(s) => {
            eprintln!("cargo install exited with {s}");
            eprintln!(
                "If v{latest} was tagged in the last few minutes it may still be propagating to crates.io; try again shortly."
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cwd_is_optional() {
        let cli = Cli::try_parse_from(["gitpane"]).unwrap();
        assert!(!cli.cwd);
    }

    #[test]
    fn cwd_flag_scans_the_current_directory() {
        let cli = Cli::try_parse_from(["gitpane", "--cwd"]).unwrap();
        assert!(cli.cwd);
    }

    #[test]
    fn cwd_does_not_consume_a_subcommand() {
        // `--cwd` is a plain boolean, so a following subcommand is not
        // swallowed as a path value.
        let cli = Cli::try_parse_from(["gitpane", "--cwd", "diagnostic"]).unwrap();
        assert!(cli.cwd);
        assert!(matches!(cli.command, Some(Command::Diagnostic)));
    }

    #[test]
    fn cwd_coexists_with_root() {
        let cli = Cli::try_parse_from(["gitpane", "--root", "~/Code", "--cwd"]).unwrap();
        assert_eq!(cli.root, Some(PathBuf::from("~/Code")));
        assert!(cli.cwd);
    }
}
