use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::config::BranchFilter;
use crate::git::graph::{DiffStat, GraphFilters, GraphRow};

/// Upper bound on cached graph snapshots. Each holds up to `MAX_COMMITS`
/// rows of owned strings plus lane vectors, on the order of a few hundred
/// KB, so the ceiling here lands in the low tens of MB — acceptable for
/// covering the repos a user actually cycles through.
pub(crate) const GRAPH_CACHE_CAPACITY: usize = 32;

/// Upper bound on cached rendered row bodies. Rows are evicted wholesale once
/// the bound is hit, so this stays far above any visible window.
pub(crate) const RENDER_CACHE_CAPACITY: usize = 256;

/// The graph-build options that shape the built rows. `label_max_len` is
/// deliberately excluded: it only changes rendering truncation, not the rows
/// themselves, so changing it must not invalidate a cached graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GraphCacheKey {
    pub(crate) path: PathBuf,
    pub(crate) branch_filter: BranchFilter,
    pub(crate) first_parent: bool,
    pub(crate) show_stats: bool,
    pub(crate) filters: GraphFilters,
}

/// A built graph snapshot, restored on a cache hit.
#[derive(Clone)]
pub(crate) struct CachedGraph {
    pub(crate) rows: Vec<GraphRow>,
    pub(crate) filter_branches: BTreeSet<String>,
    pub(crate) filter_authors: BTreeSet<String>,
}

/// Bounded per-repo cache of built graphs. Re-selecting a repo that was
/// recently viewed restores its rows from memory instead of reopening the
/// repository, re-walking up to `MAX_COMMITS` commits, and diffing trees for
/// stats. Entries are invalidated when the repo's rendered refs/HEAD move
/// (see `invalidate_repo`) and evicted least-recently-used when the cache
/// grows past `capacity`.
pub(crate) struct GraphCache {
    entries: HashMap<GraphCacheKey, GraphCacheEntry>,
    capacity: usize,
    /// Monotonic access clock for LRU eviction.
    clock: u64,
}

struct GraphCacheEntry {
    cached: CachedGraph,
    last_used: u64,
}

impl GraphCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            clock: 0,
        }
    }

    /// Look up a snapshot, bumping its LRU recency. Clones the rows so the
    /// entry stays available for the next switch back.
    pub(crate) fn get(&mut self, key: &GraphCacheKey) -> Option<CachedGraph> {
        let entry = self.entries.get_mut(key)?;
        self.clock += 1;
        entry.last_used = self.clock;
        Some(entry.cached.clone())
    }

    /// Whether an entry exists for `key`, without touching LRU recency.
    #[cfg(test)]
    pub(crate) fn contains(&self, key: &GraphCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub(crate) fn insert(&mut self, key: GraphCacheKey, cached: CachedGraph) {
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&key) {
            self.evict_lru();
        }
        self.clock += 1;
        self.entries.insert(
            key,
            GraphCacheEntry {
                cached,
                last_used: self.clock,
            },
        );
    }

    /// Fold freshly computed diff stats into the cached copy so a cache hit
    /// doesn't resurrect a graph missing its +N/-M columns.
    pub(crate) fn apply_stats(
        &mut self,
        key: &GraphCacheKey,
        stat_map: &HashMap<git2::Oid, DiffStat>,
    ) {
        if let Some(entry) = self.entries.get_mut(key) {
            for row in &mut entry.cached.rows {
                if let Some(stat) = stat_map.get(&row.oid) {
                    row.diff_stat = Some(stat.clone());
                }
            }
        }
    }

    pub(crate) fn invalidate(&mut self, path: &Path) {
        self.entries.retain(|key, _| key.path != path);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    fn evict_lru(&mut self) {
        let Some((key, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, entry)| (key.clone(), entry.last_used))
        else {
            return;
        };
        self.entries.remove(&key);
    }
}

/// Identity of a cached rendered row body: everything that can change the
/// stable part of a graph line. Horizontal scroll and the terminal width are
/// applied after the lookup each frame, and the relative-time tail is rebuilt
/// each frame, so neither is part of the key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct RowRenderKey {
    pub(crate) oid: git2::Oid,
    pub(crate) theme_generation: u64,
    pub(crate) label_max_len: usize,
    pub(crate) dimmed: bool,
    pub(crate) collapsed: bool,
}
