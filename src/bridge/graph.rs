use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::GraphConfig;
use crate::git::commit_files::batch_diff_stats;
use crate::git::graph::{
    GraphBuilder, GraphFilters, GraphOptions, GraphRefFilters, LaneSegment, MAX_COMMITS,
    compute_branch_segments, project_branch_collapses,
};

use super::{BridgeError, changes::RepositoryIdentity, commit_file_json, scope::Scope};

#[cfg(test)]
mod comparison_tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResolveRevision {
    repo: PathBuf,
    revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompareRevisions {
    repo: PathBuf,
    from: String,
    to: String,
    common_ancestor: bool,
    #[serde(default)]
    offset: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionRefs {
    repo: PathBuf,
    #[serde(default)]
    offset: usize,
}

pub(super) fn revision_refs(scope: &Scope, input: RevisionRefs) -> Result<Value, BridgeError> {
    let path = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&path)?;
    let repo = git2::Repository::open(&path)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let mut refs = Vec::new();
    for reference in repo
        .references()
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?
    {
        let reference =
            reference.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
        let name = reference
            .name()
            .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
        if !["refs/heads/", "refs/remotes/", "refs/tags/"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        let symbolic = reference
            .symbolic_target()
            .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
        let target = reference
            .target()
            .map(|oid| oid.to_string())
            .or_else(|| symbolic.map(str::to_owned))
            .ok_or_else(|| BridgeError::new("repository_error", "A ref target is unavailable."))?;
        let commit = reference
            .peel_to_commit()
            .ok()
            .map(|value| value.id().to_string());
        refs.push((name.to_owned(), target, commit));
        if refs.len() > 100_000 {
            return Err(BridgeError::new(
                "response_too_large",
                "The repository has more than 100,000 refs.",
            ));
        }
    }
    refs.sort_unstable();
    let mut hash = Sha256::new();
    for (name, target, commit) in &refs {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(target.as_bytes());
        hash.update([0]);
        if let Some(commit) = commit {
            hash.update(commit.as_bytes());
        }
        hash.update([0]);
    }
    if scope.repository(&input.repo)? != path || RepositoryIdentity::read(&path)? != identity {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while listing refs.",
        ));
    }
    let total = refs.len();
    if input.offset > total || !input.offset.is_multiple_of(256) {
        return Err(BridgeError::new("invalid_request", "Invalid ref page."));
    }
    let next = (input.offset.saturating_add(256) < total).then_some(input.offset + 256);
    Ok(
        json!({ "repo_identity": identity.fingerprint()?, "revision": crate::hash::hex(hash.finalize().as_ref()),
        "total": total, "next": next, "refs": refs.into_iter().skip(input.offset).take(256).map(|(name, _, oid)| json!({"name": name, "oid": oid})).collect::<Vec<_>>() }),
    )
}

pub(super) fn resolve_revision(
    scope: &Scope,
    input: ResolveRevision,
) -> Result<Value, BridgeError> {
    let path = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&path)?;
    if input.revision.is_empty() || input.revision.len() > 1024 || input.revision.contains('\0') {
        return Err(BridgeError::new("invalid_request", "Invalid revision."));
    }
    let repo = git2::Repository::open(&path)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let commit = repo
        .revparse_single(&input.revision)
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    if scope.repository(&input.repo)? != path || RepositoryIdentity::read(&path)? != identity {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while resolving the revision.",
        ));
    }
    Ok(json!({ "oid": commit.id().to_string(), "repo_identity": identity.fingerprint()? }))
}

pub(super) fn compare_revisions(
    scope: &Scope,
    input: CompareRevisions,
) -> Result<Value, BridgeError> {
    let path = scope.repository(&input.repo)?;
    let identity = RepositoryIdentity::read(&path)?;
    let parse = |value: &str| {
        if value.len() != 40 {
            return Err(BridgeError::new(
                "invalid_request",
                "Expected a full commit ID.",
            ));
        }
        git2::Oid::from_str(value)
            .map_err(|error| BridgeError::new("invalid_request", error.to_string()))
    };
    let from = parse(&input.from)?;
    let to = parse(&input.to)?;
    let repo = git2::Repository::open(&path)
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let base = if input.common_ancestor {
        repo.merge_base(from, to).map_err(|error| {
            BridgeError::new(
                "repository_error",
                format!("Could not determine a common ancestor: {error}"),
            )
        })?
    } else {
        from
    };
    let files = crate::git::commit_files::list_revision_files(&path, Some(base), Some(to))
        .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    if scope.repository(&input.repo)? != path || RepositoryIdentity::read(&path)? != identity {
        return Err(BridgeError::new(
            "stale_target",
            "Repository changed while comparing revisions.",
        ));
    }
    let total = files.len();
    if input.offset > total || !input.offset.is_multiple_of(256) {
        return Err(BridgeError::new(
            "invalid_request",
            "Invalid comparison page.",
        ));
    }
    let next = (input.offset.saturating_add(256) < total).then_some(input.offset + 256);
    Ok(
        json!({ "base_oid": base.to_string(), "to_oid": to.to_string(),
        "repo_identity": identity.fingerprint()?, "total": total, "next": next,
        "files": files.into_iter().skip(input.offset).take(256).map(commit_file_json).collect::<Vec<_>>() }),
    )
}

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

fn allow_worktree(scope: &Scope, failure: &RefCell<Option<BridgeError>>, path: &Path) -> bool {
    match scope.allows_missing(path) {
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
    }
}

pub(super) fn revision(
    repo: &Path,
    scope: &Scope,
    config: &GraphConfig,
) -> Result<Value, BridgeError> {
    let failure = RefCell::new(None);
    let allow = |path: &Path| allow_worktree(scope, &failure, path);
    let revision = GraphBuilder::revision_scoped(repo, &config.branches, &allow);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    let revision =
        revision.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    Ok(json!({ "revision": revision }))
}

pub(super) fn query(
    repo: &Path,
    scope: &Scope,
    config: &GraphConfig,
    first_parent: bool,
    filters: FilterRequest,
    collapsed_segments: BTreeSet<String>,
) -> Result<Value, BridgeError> {
    let options = GraphOptions {
        branch_filter: config.branches,
        label_max_len: config.label_max_len,
        show_stats: config.show_stats,
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
    };
    let failure = RefCell::new(None);
    let allow_worktree = |path: &Path| allow_worktree(scope, &failure, path);
    let branches = GraphBuilder::branch_names_scoped(repo, &options.branch_filter, &allow_worktree);
    let rows = GraphBuilder::new().build_scoped(repo, &options, &allow_worktree);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    let branches =
        branches.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let rows = rows.map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
    let oids: Vec<_> = rows.iter().map(|row| row.oid).collect();
    let stats: HashMap<_, _> = if options.show_stats {
        batch_diff_stats(repo, &oids)
            .map_err(|error| BridgeError::new("repository_error", error.to_string()))?
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
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
    // Larger values cannot truncate a JavaScript string and are not exactly
    // representable by the extension host's number type.
    let max_safe = usize::try_from(9_007_199_254_740_991_u64).unwrap_or(usize::MAX);
    Ok(
        json!({ "rows": rows, "limit": MAX_COMMITS, "branches": branches,
        "label_max_len": config.label_max_len.min(max_safe) }),
    )
}
