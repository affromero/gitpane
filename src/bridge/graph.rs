use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::git::commit_files::batch_diff_stats;
use crate::git::graph::{
    GraphBuilder, GraphFilters, GraphOptions, GraphRefFilters, LaneSegment, MAX_COMMITS,
    compute_branch_segments, project_branch_collapses,
};

use super::{BridgeError, scope::Scope};

fn lane_name(lane: &LaneSegment) -> &'static str {
    match lane {
        LaneSegment::Empty => "empty",
        LaneSegment::Straight => "straight",
        LaneSegment::Commit => "commit",
        LaneSegment::MergeLeft => "merge_left",
        LaneSegment::MergeRight => "merge_right",
        LaneSegment::ForkLeft => "fork_left",
        LaneSegment::ForkRight => "fork_right",
        LaneSegment::Horizontal => "horizontal",
        LaneSegment::CrossHorizontal => "cross_horizontal",
        LaneSegment::RightTee => "right_tee",
        LaneSegment::LeftTee => "left_tee",
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct FilterRequest {
    branches: Option<BTreeSet<String>>,
    authors: Option<BTreeSet<String>>,
    refs: RefFilterRequest,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RefFilterRequest {
    local: bool,
    remote: bool,
    tags: bool,
    stashes: bool,
}

impl Default for RefFilterRequest {
    fn default() -> Self {
        Self {
            local: true,
            remote: true,
            tags: true,
            stashes: true,
        }
    }
}

pub(super) fn query(
    repo: &Path,
    scope: &Scope,
    first_parent: bool,
    filters: FilterRequest,
    collapsed_segments: BTreeSet<String>,
) -> Result<Value, BridgeError> {
    let options = GraphOptions {
        first_parent,
        filters: GraphFilters {
            branches: filters.branches,
            authors: filters.authors,
            refs: GraphRefFilters {
                local: filters.refs.local,
                remote: filters.refs.remote,
                tags: filters.refs.tags,
                stashes: filters.refs.stashes,
            },
        },
        ..GraphOptions::default()
    };
    let failure = RefCell::new(None);
    let allow_worktree = |path: &Path| match scope.allows_missing(path) {
        Ok(false) => false,
        Ok(true) => match scope.repository(path) {
            Ok(_) => true,
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                false
            }
        },
        Err(error) => {
            *failure.borrow_mut() = Some(error);
            false
        }
    };
    let branches = GraphBuilder::branch_names_scoped(repo, &options.branch_filter, &allow_worktree);
    let rows = GraphBuilder::new().build_scoped(repo, &options, &allow_worktree);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    let branches =
        branches.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let rows = rows.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let oids: Vec<_> = rows.iter().map(|row| row.oid).collect();
    let stats: HashMap<_, _> = batch_diff_stats(repo, &oids)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?
        .into_iter()
        .collect();
    let segments = compute_branch_segments(&rows);
    let mut segment_by_oid = HashMap::new();
    for segment in &segments {
        for &index in &segment.row_indices {
            segment_by_oid.insert(rows[index].oid, segment.id.clone());
        }
    }
    let collapsed: HashSet<String> = collapsed_segments.into_iter().collect();
    let rows = project_branch_collapses(&rows, &segments, &collapsed);
    let rows: Vec<_> = rows
        .iter()
        .map(|row| {
            let labels: Vec<_> = row
                .labels
                .iter()
                .map(|label| {
                    json!({
                        "name": label.name,
                        "catalog_name": label.catalog_name,
                        "is_head": label.is_head,
                        "is_remote": label.is_remote,
                        "is_worktree": label.is_worktree,
                        "is_tag": label.is_tag,
                        "is_stash": label.is_stash,
                    })
                })
                .collect();
            let diff_stat = if row.collapsed.is_some() {
                None
            } else {
                stats
                    .get(&row.oid)
                    .map(|stat| json!({ "additions": stat.additions, "deletions": stat.deletions }))
            };
            let collapsed = row
                .collapsed
                .as_ref()
                .map(|(id, count)| json!({"id": id, "count": count}));
            json!({
                "oid": row.oid.to_string(),
                "short_id": row.short_id,
                "message": row.message,
                "author": row.author,
                "time": row.time,
                "is_merge": row.is_merge,
                "commit_col": row.commit_col,
                "lanes": row.lanes.iter().map(lane_name).collect::<Vec<_>>(),
                "parent_oids": row.parent_oids.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "horizontal_spans": row.horizontal_spans,
                "labels": labels,
                "diff_stat": diff_stat,
                "segment_id": segment_by_oid.get(&row.oid),
                "collapsed": collapsed,
            })
        })
        .collect();
    Ok(json!({ "rows": rows, "limit": MAX_COMMITS, "branches": branches }))
}
