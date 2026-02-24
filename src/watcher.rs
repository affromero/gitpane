use notify_debouncer_full::{
    DebounceEventResult, Debouncer, NoCache, new_debouncer_opt,
    notify::{Config, RecommendedWatcher, RecursiveMode},
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

pub(crate) struct RepoWatcher {
    _debouncer: Debouncer<RecommendedWatcher, NoCache>,
}

impl RepoWatcher {
    pub fn new(
        repo_paths: &[PathBuf],
        debounce_ms: u64,
        event_tx: UnboundedSender<Event>,
    ) -> color_eyre::Result<Self> {
        let indexed_paths: Vec<(usize, PathBuf)> = repo_paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.clone()))
            .collect();

        // Bridge channel: notify callback (OS thread) -> tokio task
        let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<PathBuf>>();

        // Spawn tokio task to route changed paths to repo indices.
        // Filters out .git/ internals to prevent feedback loops (git2 reads
        // trigger watcher events which would re-trigger git2 queries).
        let paths_for_routing = indexed_paths.clone();
        tokio::spawn(async move {
            while let Some(changed_paths) = bridge_rx.recv().await {
                let mut affected_repos = HashSet::new();

                for changed_path in &changed_paths {
                    // Allow key .git/ files that change on commit/pull/checkout,
                    // but skip noisy internals that cause feedback loops with git2.
                    if changed_path.components().any(|c| c.as_os_str() == ".git") {
                        let name = changed_path
                            .file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default();
                        let path_str = changed_path.to_string_lossy();
                        let is_meaningful = name == "HEAD"
                            || name == "index"
                            || name == "MERGE_HEAD"
                            || name == "REBASE_HEAD"
                            || name == "COMMIT_EDITMSG"
                            || path_str.contains(".git/refs/");
                        if !is_meaningful {
                            continue;
                        }
                    }

                    for (idx, repo_path) in &paths_for_routing {
                        if changed_path.starts_with(repo_path) {
                            affected_repos.insert(*idx);
                            break;
                        }
                    }
                }

                for idx in affected_repos {
                    let _ = event_tx.send(Event::RepoChanged(idx));
                }
            }
        });

        let config = Config::default().with_poll_interval(Duration::from_secs(2));

        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            Duration::from_millis(debounce_ms),
            None,
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    let paths: Vec<PathBuf> =
                        events.into_iter().flat_map(|e| e.event.paths).collect();
                    if !paths.is_empty() {
                        let _ = bridge_tx.send(paths);
                    }
                }
            },
            NoCache,
            config,
        )?;

        // Watch each repo root recursively
        for (_idx, path) in &indexed_paths {
            if path.exists()
                && let Err(e) = debouncer.watch(path, RecursiveMode::Recursive)
            {
                tracing::warn!("Failed to watch {}: {}", path.display(), e);
            }
        }

        Ok(Self {
            _debouncer: debouncer,
        })
    }
}
