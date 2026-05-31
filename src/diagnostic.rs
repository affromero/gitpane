use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeInfo<'a> {
    pub version: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
    pub available_parallelism: usize,
}

impl<'a> RuntimeInfo<'a> {
    pub(crate) fn current(version: &'a str) -> Self {
        Self {
            version,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            available_parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        }
    }
}

pub(crate) fn render(config: &Config, repos: &[PathBuf], runtime: RuntimeInfo<'_>) -> String {
    let mut out = String::new();

    push_line(&mut out, "gitpane diagnostic");
    push_line(&mut out, &format!("version: {}", runtime.version));
    push_line(
        &mut out,
        &format!("platform: {} {}", runtime.os, runtime.arch),
    );
    push_line(
        &mut out,
        &format!("available_parallelism: {}", runtime.available_parallelism),
    );
    out.push('\n');

    push_line(&mut out, "config:");
    push_line(
        &mut out,
        &format!(
            "  loaded: {}",
            display_optional_path(config.loaded_path.as_deref())
        ),
    );
    push_line(
        &mut out,
        &format!(
            "  write_target: {}",
            display_optional_path(config.write_target_override.as_deref())
        ),
    );
    push_line(
        &mut out,
        &format!("  theme: {}", config.effective_theme_name()),
    );
    push_line(&mut out, &format!("  scan_depth: {}", config.scan_depth));
    push_line(
        &mut out,
        &format!("  root_dirs: {}", format_paths(&config.root_dirs)),
    );
    push_line(
        &mut out,
        &format!("  pinned_repos: {}", config.pinned_repos.len()),
    );
    push_line(
        &mut out,
        &format!("  excluded_repos: {}", config.excluded_repos.len()),
    );
    out.push('\n');

    push_line(&mut out, "watch:");
    push_line(
        &mut out,
        &format!("  debounce_ms: {}", config.watch.debounce_ms),
    );
    push_line(
        &mut out,
        &format!(
            "  refresh_cooldown_ms: {}",
            config.watch.refresh_cooldown_ms
        ),
    );
    push_line(
        &mut out,
        &format!(
            "  watch_worktree_dirs: {}",
            config.watch.watch_worktree_dirs
        ),
    );
    push_line(
        &mut out,
        &format!("  poll_local_secs: {}", config.watch.poll_local_secs),
    );
    push_line(
        &mut out,
        &format!("  poll_fetch_secs: {}", config.watch.poll_fetch_secs),
    );
    push_line(
        &mut out,
        &format!(
            "  max_concurrent_polls: {}",
            config.watch.max_concurrent_polls
        ),
    );
    push_line(
        &mut out,
        &format!(
            "  discovery_cooldown_secs: {}",
            config.watch.discovery_cooldown_secs
        ),
    );
    push_line(
        &mut out,
        &format!(
            "  watch_exclude_dirs: {}",
            config.watch.watch_exclude_dirs.join(", ")
        ),
    );
    out.push('\n');

    push_line(&mut out, "graph:");
    push_line(
        &mut out,
        &format!("  branches: {:?}", config.graph.branches),
    );
    push_line(
        &mut out,
        &format!("  show_stats: {}", config.graph.show_stats),
    );
    out.push('\n');

    push_line(&mut out, "workspace:");
    push_line(&mut out, &format!("  repos_discovered: {}", repos.len()));
    push_line(
        &mut out,
        &format!("  missing_roots: {}", format_paths(&missing_roots(config))),
    );
    out.push('\n');

    let warnings = warnings(config, repos, runtime);
    push_line(&mut out, "warnings:");
    if warnings.is_empty() {
        push_line(&mut out, "  none");
    } else {
        for warning in warnings {
            push_line(&mut out, &format!("  - {warning}"));
        }
    }

    out
}

fn warnings(config: &Config, repos: &[PathBuf], runtime: RuntimeInfo<'_>) -> Vec<String> {
    let mut warnings = Vec::new();

    if config.watch.refresh_cooldown_ms < 1000 {
        warnings.push(format!(
            "watch.refresh_cooldown_ms={} can allow rapid watcher-triggered status loops",
            config.watch.refresh_cooldown_ms
        ));
    }

    if config.watch.watch_worktree_dirs && config.watch.refresh_cooldown_ms < 5000 {
        warnings.push(
            "watch.watch_worktree_dirs=true with a short refresh cooldown can increase CPU during file storms"
                .to_string(),
        );
    }

    if config.watch.poll_local_secs == 0 {
        warnings.push("watch.poll_local_secs=0 makes local polling continuous".to_string());
    }

    if config.watch.max_concurrent_polls > runtime.available_parallelism {
        warnings.push(format!(
            "watch.max_concurrent_polls={} exceeds available_parallelism={}",
            config.watch.max_concurrent_polls, runtime.available_parallelism
        ));
    }

    if config.graph.show_stats && repos.len() > 50 {
        warnings.push(format!(
            "graph.show_stats=true with {} repos can add background graph work",
            repos.len()
        ));
    }

    if repos.len() > 100 {
        warnings.push(format!(
            "{} repos discovered; consider narrower root_dirs or scan_depth",
            repos.len()
        ));
    }

    for root in missing_roots(config) {
        warnings.push(format!("root_dir does not exist: {}", root.display()));
    }

    warnings
}

fn missing_roots(config: &Config) -> Vec<PathBuf> {
    config
        .root_dirs
        .iter()
        .filter(|root| !root.exists())
        .cloned()
        .collect()
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        "none".to_string()
    } else {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn display_optional_path(path: Option<&Path>) -> String {
    path.map(|p| p.display().to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> RuntimeInfo<'static> {
        RuntimeInfo {
            version: "test",
            os: "test-os",
            arch: "test-arch",
            available_parallelism: 2,
        }
    }

    #[test]
    fn render_includes_operational_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            root_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        config.watch.refresh_cooldown_ms = 5000;
        config.watch.max_concurrent_polls = 2;

        let output = render(&config, &[], runtime());

        assert!(output.contains("gitpane diagnostic"));
        assert!(output.contains("version: test"));
        assert!(output.contains("platform: test-os test-arch"));
        assert!(output.contains("refresh_cooldown_ms: 5000"));
        assert!(output.contains("watch_worktree_dirs: false"));
        assert!(output.contains("repos_discovered: 0"));
        assert!(output.contains("warnings:\n  none"));
    }

    #[test]
    fn render_warns_about_cpu_pressure_settings() {
        let mut config = Config::default();
        config.watch.refresh_cooldown_ms = 250;
        config.watch.watch_worktree_dirs = true;
        config.watch.poll_local_secs = 0;
        config.watch.max_concurrent_polls = 8;

        let repos: Vec<PathBuf> = (0..60)
            .map(|i| PathBuf::from(format!("/repo-{i}")))
            .collect();
        let output = render(&config, &repos, runtime());

        assert!(output.contains("watch.refresh_cooldown_ms=250"));
        assert!(output.contains("watch.watch_worktree_dirs=true"));
        assert!(output.contains("watch.poll_local_secs=0"));
        assert!(output.contains("watch.max_concurrent_polls=8"));
        assert!(output.contains("graph.show_stats=true with 60 repos"));
    }
}
