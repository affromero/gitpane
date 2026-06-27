use git2::Oid;
use std::collections::{HashMap, HashSet};

use super::GraphRow;

#[derive(Clone, Debug)]
pub(crate) struct BranchSegment {
    pub id: String,
    pub display_name: String,
    pub row_indices: Vec<usize>,
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            // Union by smaller index so the tip (earliest row) becomes root
            if ra < rb {
                self.parent[rb] = ra;
            } else {
                self.parent[ra] = rb;
            }
        }
    }
}

pub(crate) fn compute_branch_segments(rows: &[GraphRow]) -> Vec<BranchSegment> {
    if rows.is_empty() {
        return Vec::new();
    }

    let oid_to_idx: HashMap<Oid, usize> =
        rows.iter().enumerate().map(|(i, r)| (r.oid, i)).collect();

    // Mark main trunk: walk first-parent chain from row 0
    let mut main_trunk: HashSet<usize> = HashSet::new();
    let mut cur = 0usize;
    loop {
        main_trunk.insert(cur);
        let first_parent = rows[cur].parent_oids.first();
        match first_parent.and_then(|oid| oid_to_idx.get(oid)) {
            Some(&next_idx) => cur = next_idx,
            None => break,
        }
    }

    // Union-Find over non-trunk rows via first-parent links
    let mut uf = UnionFind::new(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if main_trunk.contains(&i) {
            continue;
        }
        if let Some(&parent_idx) = row.parent_oids.first().and_then(|oid| oid_to_idx.get(oid))
            && !main_trunk.contains(&parent_idx)
        {
            uf.union(i, parent_idx);
        }
    }

    // Group by union-find root
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..rows.len() {
        if main_trunk.contains(&i) {
            continue;
        }
        let root = uf.find(i);
        groups.entry(root).or_default().push(i);
    }

    let mut segments: Vec<BranchSegment> = Vec::new();
    for (_, mut indices) in groups {
        indices.sort();
        let tip_idx = indices[0];
        let tip = &rows[tip_idx];
        let display_name = tip
            .labels
            .iter()
            .find(|l| !l.is_tag)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| tip.short_id.clone());
        segments.push(BranchSegment {
            id: tip.oid.to_string(),
            display_name,
            row_indices: indices,
        });
    }

    // Sort segments by their first row index for deterministic ordering
    segments.sort_by_key(|s| s.row_indices[0]);
    segments
}
