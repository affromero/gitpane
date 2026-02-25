use git2::{BranchType, Oid, Repository, Sort};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::BranchFilter;

const MAX_COMMITS: usize = 200;
const PALETTE_SIZE: usize = 6;

#[derive(Clone, Debug)]
pub(crate) struct BranchLabel {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub is_worktree: bool,
    pub is_tag: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct GraphOptions {
    pub branch_filter: BranchFilter,
    pub label_max_len: usize,
    pub first_parent: bool,
    pub show_stats: bool,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            branch_filter: BranchFilter::All,
            label_max_len: 24,
            first_parent: false,
            show_stats: true,
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
    pub diff_stat: Option<DiffStat>,
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
        let repo = Repository::open(path)?;
        let mut ref_map = resolve_refs(&repo, &options.branch_filter);

        let mut revwalk = repo.revwalk()?;
        revwalk.push_head().ok(); // ok: handles unborn HEAD
        for &oid in ref_map.keys() {
            revwalk.push(oid).ok(); // git2 deduplicates
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
            let message = commit.summary().unwrap_or("").to_string();
            let author = commit.author().name().unwrap_or("").to_string();
            let time = commit.time().seconds();

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
                diff_stat: None,
            });
        }

        Ok(rows)
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

fn resolve_refs(repo: &Repository, filter: &BranchFilter) -> HashMap<Oid, Vec<BranchLabel>> {
    if *filter == BranchFilter::None {
        return HashMap::new();
    }

    let head_oid = repo.head().ok().and_then(|r| r.target());
    let head_name = repo
        .head()
        .ok()
        .and_then(|r| r.shorthand().map(String::from));
    let wt_branches = collect_worktree_branches(repo);

    let mut map: HashMap<Oid, Vec<BranchLabel>> = HashMap::new();

    let branch_types: Vec<BranchType> = match filter {
        BranchFilter::All => vec![BranchType::Local, BranchType::Remote],
        BranchFilter::Local => vec![BranchType::Local],
        BranchFilter::Remote => vec![BranchType::Remote],
        BranchFilter::None => unreachable!(),
    };

    for bt in branch_types {
        let branches = match repo.branches(Some(bt)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for branch_result in branches {
            let (branch, _) = match branch_result {
                Ok(b) => b,
                Err(_) => continue,
            };
            let target = match branch.get().target() {
                Some(oid) => oid,
                None => continue,
            };
            let name = match branch.name() {
                Ok(Some(n)) => n.to_string(),
                _ => continue,
            };
            let is_remote = bt == BranchType::Remote;
            let is_head =
                !is_remote && head_oid == Some(target) && head_name.as_deref() == Some(&name);
            let is_worktree = !is_remote && wt_branches.contains(&name);

            map.entry(target).or_default().push(BranchLabel {
                name,
                is_head,
                is_remote,
                is_worktree,
                is_tag: false,
            });
        }
    }

    // Tags
    if let Ok(tag_names) = repo.tag_names(None) {
        for name in tag_names.iter().flatten() {
            let refname = format!("refs/tags/{}", name);
            let Ok(reference) = repo.find_reference(&refname) else {
                continue;
            };
            let oid = reference
                .peel_to_commit()
                .ok()
                .map(|c| c.id())
                .or_else(|| reference.target());
            if let Some(oid) = oid {
                map.entry(oid).or_default().push(BranchLabel {
                    name: name.to_string(),
                    is_head: false,
                    is_remote: false,
                    is_worktree: false,
                    is_tag: true,
                });
            }
        }
    }

    // Sort: HEAD first, then local, then remote, then tags, then alphabetical
    for labels in map.values_mut() {
        labels.sort_by(|a, b| {
            b.is_head
                .cmp(&a.is_head)
                .then(a.is_tag.cmp(&b.is_tag))
                .then(a.is_remote.cmp(&b.is_remote))
                .then(a.name.cmp(&b.name))
        });
    }

    map
}

fn collect_worktree_branches(repo: &Repository) -> HashSet<String> {
    let mut branches = HashSet::new();
    let wt_names = match repo.worktrees() {
        Ok(names) => names,
        Err(_) => return branches,
    };
    for i in 0..wt_names.len() {
        let name = match wt_names.get(i) {
            Some(n) => n,
            None => continue,
        };
        let wt = match repo.find_worktree(name) {
            Ok(wt) => wt,
            Err(_) => continue,
        };
        let wt_repo = match Repository::open(wt.path()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Ok(head) = wt_repo.head()
            && let Some(shorthand) = head.shorthand()
        {
            branches.insert(shorthand.to_string());
        }
    }
    branches
}

/// Assign a color index (0..PALETTE_SIZE) for a given lane column.
/// Adjacent lanes get different colors.
pub(crate) fn lane_color(col: usize) -> usize {
    col % PALETTE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::TempDir;

    fn create_commit(repo: &Repository, message: &str, parents: &[&git2::Commit]) -> Oid {
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, parents)
            .unwrap()
    }

    #[test]
    fn test_linear_history_single_lane() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let oid1 = create_commit(&repo, "first", &[]);
        let c1 = repo.find_commit(oid1).unwrap();
        let _oid2 = create_commit(&repo, "second", &[&c1]);

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        assert_eq!(rows.len(), 2);
        // All commits should be in column 0
        for row in &rows {
            assert_eq!(row.commit_col, 0);
        }
    }

    #[test]
    fn test_merge_creates_two_lanes() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let oid1 = create_commit(&repo, "root", &[]);
        let c1 = repo.find_commit(oid1).unwrap();

        // Create two divergent commits
        let sig = Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let oid2 = repo
            .commit(None, &sig, &sig, "branch-a", &tree, &[&c1])
            .unwrap();
        let c2 = repo.find_commit(oid2).unwrap();

        let oid3 = repo
            .commit(None, &sig, &sig, "branch-b", &tree, &[&c1])
            .unwrap();
        let c3 = repo.find_commit(oid3).unwrap();

        // Merge: first parent is c2
        let merge_oid = repo
            .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
            .unwrap();
        repo.set_head_detached(merge_oid).unwrap();

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        assert!(rows.len() >= 3);
        let merge_row = &rows[0];
        assert!(!merge_row.lanes.is_empty());
    }

    #[test]
    fn test_root_commit_closes_lane() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let _oid1 = create_commit(&repo, "only", &[]);

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].commit_col, 0);
        assert_eq!(rows[0].lanes[0], LaneSegment::Commit);
    }

    #[test]
    fn test_multiple_branches_assign_different_columns() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let oid1 = create_commit(&repo, "root", &[]);
        let c1 = repo.find_commit(oid1).unwrap();

        let sig = Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let oid2 = repo
            .commit(None, &sig, &sig, "left", &tree, &[&c1])
            .unwrap();
        let c2 = repo.find_commit(oid2).unwrap();

        let oid3 = repo
            .commit(None, &sig, &sig, "right", &tree, &[&c1])
            .unwrap();
        let c3 = repo.find_commit(oid3).unwrap();

        let merge_oid = repo
            .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
            .unwrap();
        repo.set_head_detached(merge_oid).unwrap();

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        // After merge, we should see a fork to a second column
        let merge_row = &rows[0];
        assert!(
            merge_row.lanes.len() >= 2,
            "Expected >= 2 lanes at merge, got {}",
            merge_row.lanes.len()
        );
    }

    #[test]
    fn test_graph_rows_carry_labels() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let oid1 = create_commit(&repo, "first", &[]);
        let c1 = repo.find_commit(oid1).unwrap();
        let _oid2 = create_commit(&repo, "second", &[&c1]);

        // HEAD is on the default branch — tip commit should have a label
        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        let tip = &rows[0];
        assert!(
            !tip.labels.is_empty(),
            "tip commit should have at least one branch label"
        );
    }

    #[test]
    fn test_head_marked() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let _oid1 = create_commit(&repo, "init", &[]);

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        let head_labels: Vec<_> = rows[0].labels.iter().filter(|l| l.is_head).collect();
        assert_eq!(head_labels.len(), 1, "exactly one label should be HEAD");
    }

    #[test]
    fn test_merge_is_merge_true() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let oid1 = create_commit(&repo, "root", &[]);
        let c1 = repo.find_commit(oid1).unwrap();

        let sig = Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let oid2 = repo
            .commit(None, &sig, &sig, "branch-a", &tree, &[&c1])
            .unwrap();
        let c2 = repo.find_commit(oid2).unwrap();

        let oid3 = repo
            .commit(None, &sig, &sig, "branch-b", &tree, &[&c1])
            .unwrap();
        let c3 = repo.find_commit(oid3).unwrap();

        let merge_oid = repo
            .commit(None, &sig, &sig, "merge", &tree, &[&c2, &c3])
            .unwrap();
        repo.set_head_detached(merge_oid).unwrap();

        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &GraphOptions::default()).unwrap();

        assert!(rows[0].is_merge, "first row should be a merge commit");
        assert!(
            !rows[1].is_merge,
            "non-merge commit should have is_merge=false"
        );
    }

    #[test]
    fn test_merge_left_horizontal_fill() {
        let mut builder = GraphBuilder::new();
        let oid_target = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let oid_b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let oid_c = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();
        let oid_commit = Oid::from_str("dddddddddddddddddddddddddddddddddddddd").unwrap();

        builder.active_lanes = vec![Some(oid_target), Some(oid_b), Some(oid_c), Some(oid_commit)];

        let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_target]);

        assert_eq!(col, 3);
        assert_eq!(lanes[0], LaneSegment::RightTee);
        assert_eq!(lanes[1], LaneSegment::CrossHorizontal);
        assert_eq!(lanes[2], LaneSegment::CrossHorizontal);
        assert_eq!(lanes[3], LaneSegment::MergeLeft);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], (0, 3, lane_color(3)));
    }

    #[test]
    fn test_fork_right_horizontal_fill() {
        let mut builder = GraphBuilder::new();
        let oid_commit = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let oid_active = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let oid_parent1 = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();
        let oid_parent2 = Oid::from_str("dddddddddddddddddddddddddddddddddddddd").unwrap();

        builder.active_lanes = vec![Some(oid_commit), Some(oid_active), None];

        let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_parent1, oid_parent2]);

        assert_eq!(col, 0);
        assert_eq!(lanes[0], LaneSegment::Commit);
        assert_eq!(lanes[1], LaneSegment::CrossHorizontal);
        assert_eq!(lanes[2], LaneSegment::ForkRight);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], (0, 2, lane_color(2)));
    }

    #[test]
    fn test_adjacent_merge_no_intermediate() {
        let mut builder = GraphBuilder::new();
        let oid_target = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let oid_commit = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();

        builder.active_lanes = vec![Some(oid_target), Some(oid_commit)];

        let (col, lanes, spans) = builder.process_commit(oid_commit, &[oid_target]);

        assert_eq!(col, 1);
        assert_eq!(lanes[0], LaneSegment::RightTee);
        assert_eq!(lanes[1], LaneSegment::MergeLeft);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], (0, 1, lane_color(1)));
    }

    #[test]
    fn test_filter_none_yields_no_labels() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();

        let _oid1 = create_commit(&repo, "init", &[]);

        let options = GraphOptions {
            branch_filter: BranchFilter::None,
            ..Default::default()
        };
        let builder = GraphBuilder::new();
        let rows = builder.build(tmp.path(), &options).unwrap();

        for row in &rows {
            assert!(
                row.labels.is_empty(),
                "filter=None should produce no labels"
            );
        }
    }
}
