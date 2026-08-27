use git2::{Oid, Repository, Sort};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use crate::config::BranchFilter;

mod refs;
mod segments;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_filters;

use refs::{merge_stash_labels, resolve_refs};

pub(crate) use segments::{BranchSegment, compute_branch_segments};

const MAX_COMMITS: usize = 200;
const PALETTE_SIZE: usize = 6;

#[derive(Clone, Debug)]
pub(crate) struct BranchLabel {
    pub name: String,
    /// Name shown in the branch filter picker. A local branch and the
    /// remote-tracking branch it tracks (its upstream) share one catalog name,
    /// so they collapse to a single filter entry and selecting it walks from
    /// both tips. Most labels use their own `name` as the catalog name.
    ///
    /// Catalog identity is a plain string, so a local branch literally named
    /// like a remote shorthand (a local `origin/topic` next to an untracked
    /// remote `origin/topic`) shares one entry and selecting it walks both
    /// tips. This predates the upstream collapse (filter matching was always
    /// string-based); the upgrade path is ref-qualified identities
    /// (`refs/heads/...` vs `refs/remotes/...`) with separate display names.
    pub catalog_name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub is_worktree: bool,
    pub is_tag: bool,
    /// Label points at a commit that is the parent of a stash entry
    /// (`stash@{n}`). Rendered with the stash theme color.
    pub is_stash: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphOptions {
    pub branch_filter: BranchFilter,
    pub label_max_len: usize,
    pub first_parent: bool,
    pub show_stats: bool,
    pub filters: GraphFilters,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            branch_filter: BranchFilter::All,
            label_max_len: 24,
            first_parent: false,
            show_stats: true,
            filters: GraphFilters::default(),
        }
    }
}

/// Optional include lists for the graph. `None` means every value in the
/// category is included; an empty set deliberately produces an empty graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct GraphFilters {
    pub branches: Option<BTreeSet<String>>,
    pub authors: Option<BTreeSet<String>>,
    pub refs: GraphRefFilters,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GraphRefFilters {
    pub local: bool,
    pub remote: bool,
    pub tags: bool,
    pub stashes: bool,
}

impl Default for GraphRefFilters {
    fn default() -> Self {
        Self {
            local: true,
            remote: true,
            tags: true,
            stashes: true,
        }
    }
}

impl GraphRefFilters {
    fn includes(&self, label: &BranchLabel) -> bool {
        if label.is_stash {
            self.stashes
        } else if label.is_tag {
            self.tags
        } else if label.is_remote {
            self.remote
        } else {
            self.local
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DiffStat {
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct GraphRow {
    pub commit_col: usize,
    pub lanes: Vec<LaneSegment>,
    pub oid: Oid,
    pub short_id: String,
    pub message: String,
    pub author: String,
    pub time: i64,
    pub labels: Vec<BranchLabel>,
    pub is_merge: bool,
    pub horizontal_spans: Vec<(usize, usize, usize)>,
    pub parent_oids: Vec<Oid>,
    pub diff_stat: Option<DiffStat>,
    /// If set, this row is a collapsed-branch placeholder: (branch_name, hidden_count).
    pub collapsed: Option<(String, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LaneSegment {
    Empty,
    Straight,
    Commit,
    MergeLeft,
    MergeRight,
    ForkLeft,
    ForkRight,
    Horizontal,
    CrossHorizontal,
    RightTee,
    LeftTee,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphBuilder {
    active_lanes: Vec<Option<Oid>>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            active_lanes: Vec::new(),
        }
    }

    pub fn build(
        mut self,
        path: &Path,
        options: &GraphOptions,
    ) -> color_eyre::Result<Vec<GraphRow>> {
        let mut repo = Repository::open(path)?;
        let mut ref_map = resolve_refs(&repo, &options.branch_filter);
        merge_stash_labels(&mut repo, &mut ref_map);

        // A branch selection is the graph's root set. Keep this separate from
        // label visibility: tags/stashes may decorate selected commits, but
        // must not keep an otherwise empty branch selection alive.
        let selected_branch_oids = options.filters.branches.as_ref().map(|branches| {
            ref_map
                .iter()
                .filter(|(_, labels)| {
                    labels.iter().any(|label| {
                        !label.is_tag
                            && !label.is_stash
                            && options.filters.refs.includes(label)
                            && branches.contains(&label.catalog_name)
                    })
                })
                .map(|(oid, _)| *oid)
                .collect::<HashSet<_>>()
        });
        if selected_branch_oids.as_ref().is_some_and(HashSet::is_empty) {
            return Ok(Vec::new());
        }

        for labels in ref_map.values_mut() {
            labels.retain(|label| options.filters.refs.includes(label));
        }
        ref_map.retain(|_, labels| !labels.is_empty());

        if let Some(branches) = &options.filters.branches {
            for labels in ref_map.values_mut() {
                labels.retain(|label| {
                    label.is_tag || label.is_stash || branches.contains(&label.catalog_name)
                });
            }
            ref_map.retain(|_, labels| !labels.is_empty());
        }

        let mut revwalk = repo.revwalk()?;
        // An explicit branch selection owns the walk roots. Otherwise retain
        // the usual HEAD root so detached HEAD and unborn repositories behave
        // exactly as before.
        if options.filters.branches.is_none() && options.filters.refs.local {
            revwalk.push_head().ok(); // ok: handles unborn HEAD
        }
        if let Some(oids) = &selected_branch_oids {
            for &oid in oids {
                revwalk.push(oid).ok(); // git2 deduplicates
            }
        } else {
            for &oid in ref_map.keys() {
                revwalk.push(oid).ok(); // git2 deduplicates
            }
        }
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        if options.first_parent {
            revwalk.simplify_first_parent()?;
        }

        let mut rows = Vec::new();

        for oid_result in revwalk.take(MAX_COMMITS) {
            let oid = oid_result?;
            let commit = repo.find_commit(oid)?;

            let parent_oids: Vec<Oid> = commit.parent_ids().collect();
            let is_merge = commit.parent_count() > 1;
            let labels = ref_map.remove(&oid).unwrap_or_default();
            let (commit_col, lanes, horizontal_spans) = self.process_commit(oid, &parent_oids);

            let short_id = oid.to_string()[..7].to_string();
            let message = commit.summary().ok().flatten().unwrap_or("").to_string();
            let author = commit.author().name().unwrap_or("").to_string();
            let time = commit.time().seconds();

            // Still process excluded commits above so the lane state for later
            // visible commits remains consistent with the repository DAG.
            if options
                .filters
                .authors
                .as_ref()
                .is_some_and(|authors| !authors.contains(&author))
            {
                continue;
            }

            rows.push(GraphRow {
                commit_col,
                lanes,
                oid,
                short_id,
                message,
                author,
                time,
                labels,
                is_merge,
                horizontal_spans,
                parent_oids,
                diff_stat: None,
                collapsed: None,
            });
        }

        Ok(rows)
    }

    /// Branch names available to the filter picker, independent of the active
    /// graph filters. A filtered walk must not make deselected branches vanish
    /// from the picker.
    pub fn branch_names(
        path: &Path,
        branch_filter: &BranchFilter,
    ) -> color_eyre::Result<Vec<String>> {
        let repo = Repository::open(path)?;
        let refs = resolve_refs(&repo, branch_filter);
        let names = refs
            .values()
            .flat_map(|labels| labels.iter())
            .filter(|label| !label.is_tag && !label.is_stash)
            .map(|label| label.catalog_name.clone())
            .collect::<BTreeSet<_>>();
        Ok(names.into_iter().collect())
    }

    fn process_commit(
        &mut self,
        oid: Oid,
        parent_oids: &[Oid],
    ) -> (usize, Vec<LaneSegment>, Vec<(usize, usize, usize)>) {
        // Find which lane this commit occupies
        let commit_col = self
            .active_lanes
            .iter()
            .position(|lane| *lane == Some(oid))
            .unwrap_or_else(|| {
                // Allocate a new lane
                let col = self.find_free_lane();
                if col < self.active_lanes.len() {
                    self.active_lanes[col] = Some(oid);
                } else {
                    self.active_lanes.push(Some(oid));
                }
                col
            });

        // Build lane segments for this row
        let lane_count = self.active_lanes.len().max(commit_col + 1);
        let mut lanes = vec![LaneSegment::Empty; lane_count];

        // Mark continuing lanes
        for (i, lane) in self.active_lanes.iter().enumerate() {
            if i < lanes.len() && lane.is_some() && i != commit_col {
                lanes[i] = LaneSegment::Straight;
            }
        }

        // Mark commit position
        lanes[commit_col] = LaneSegment::Commit;

        // Process parents
        // Clear this commit's lane first
        self.active_lanes[commit_col] = None;
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();

        if !parent_oids.is_empty() {
            // First parent continues in same lane
            let first_parent = parent_oids[0];

            // Check if first parent is already in another lane
            let existing_lane = self
                .active_lanes
                .iter()
                .position(|lane| *lane == Some(first_parent));

            if let Some(existing) = existing_lane {
                // First parent already has a lane — merge to it
                if existing < commit_col {
                    lanes[commit_col] = LaneSegment::MergeLeft;
                    spans.push((existing, commit_col, lane_color(commit_col)));
                } else if existing > commit_col {
                    lanes[commit_col] = LaneSegment::MergeRight;
                    spans.push((commit_col, existing, lane_color(commit_col)));
                }
                // Don't re-assign; lane stays as is
            } else {
                // First parent takes over this lane
                self.active_lanes[commit_col] = Some(first_parent);
            }

            // Additional parents fork into new lanes
            for &parent_oid in &parent_oids[1..] {
                let existing = self
                    .active_lanes
                    .iter()
                    .position(|lane| *lane == Some(parent_oid));

                if existing.is_none() {
                    let new_col = self.find_free_lane();
                    if new_col < self.active_lanes.len() {
                        self.active_lanes[new_col] = Some(parent_oid);
                    } else {
                        self.active_lanes.push(Some(parent_oid));
                    }
                    // Extend lanes if needed
                    while lanes.len() <= new_col {
                        lanes.push(LaneSegment::Empty);
                    }
                    if new_col > commit_col {
                        lanes[new_col] = LaneSegment::ForkRight;
                        spans.push((commit_col, new_col, lane_color(new_col)));
                    } else {
                        lanes[new_col] = LaneSegment::ForkLeft;
                        spans.push((new_col, commit_col, lane_color(new_col)));
                    }
                }
            }
        }

        // Horizontal fill: connect merge/fork endpoints with ─ and ┼
        for &(left, right, _) in &spans {
            if lanes[left] == LaneSegment::Straight {
                lanes[left] = LaneSegment::RightTee;
            }
            if right < lanes.len() && lanes[right] == LaneSegment::Straight {
                lanes[right] = LaneSegment::LeftTee;
            }
            for col in (left + 1)..right {
                if col < lanes.len() {
                    if lanes[col] == LaneSegment::Straight {
                        lanes[col] = LaneSegment::CrossHorizontal;
                    } else if lanes[col] == LaneSegment::Empty {
                        lanes[col] = LaneSegment::Horizontal;
                    }
                }
            }
        }

        // Compact: remove trailing empty lanes
        while self.active_lanes.last() == Some(&None) {
            self.active_lanes.pop();
        }

        (commit_col, lanes, spans)
    }

    fn find_free_lane(&self) -> usize {
        self.active_lanes
            .iter()
            .position(|lane| lane.is_none())
            .unwrap_or(self.active_lanes.len())
    }
}

/// Assign a color index (0..PALETTE_SIZE) for a given lane column.
/// Adjacent lanes get different colors.
pub(crate) fn lane_color(col: usize) -> usize {
    col % PALETTE_SIZE
}
