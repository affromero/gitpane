use git2::{Oid, Repository, Sort};
use std::path::Path;

const MAX_COMMITS: usize = 200;
const PALETTE_SIZE: usize = 6;

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

    pub fn build(mut self, path: &Path) -> color_eyre::Result<Vec<GraphRow>> {
        let repo = Repository::open(path)?;
        let mut revwalk = repo.revwalk()?;
        revwalk.push_head()?;
        revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

        let mut rows = Vec::new();

        for oid_result in revwalk.take(MAX_COMMITS) {
            let oid = oid_result?;
            let commit = repo.find_commit(oid)?;

            let parent_oids: Vec<Oid> = commit.parent_ids().collect();
            let row = self.process_commit(oid, &parent_oids);

            let short_id = oid.to_string()[..7].to_string();
            let message = commit.summary().unwrap_or("").to_string();
            let author = commit.author().name().unwrap_or("").to_string();
            let time = commit.time().seconds();

            rows.push(GraphRow {
                commit_col: row.0,
                lanes: row.1,
                oid,
                short_id,
                message,
                author,
                time,
            });
        }

        Ok(rows)
    }

    fn process_commit(&mut self, oid: Oid, parent_oids: &[Oid]) -> (usize, Vec<LaneSegment>) {
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
                } else if existing > commit_col {
                    lanes[commit_col] = LaneSegment::MergeRight;
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
                    } else {
                        lanes[new_col] = LaneSegment::ForkLeft;
                    }
                }
            }
        }

        // Compact: remove trailing empty lanes
        while self.active_lanes.last() == Some(&None) {
            self.active_lanes.pop();
        }

        (commit_col, lanes)
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
        let rows = builder.build(tmp.path()).unwrap();

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
        let rows = builder.build(tmp.path()).unwrap();

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
        let rows = builder.build(tmp.path()).unwrap();

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
        let rows = builder.build(tmp.path()).unwrap();

        // After merge, we should see a fork to a second column
        let merge_row = &rows[0];
        assert!(
            merge_row.lanes.len() >= 2,
            "Expected >= 2 lanes at merge, got {}",
            merge_row.lanes.len()
        );
    }
}
