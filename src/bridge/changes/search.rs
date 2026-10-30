use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Scope, Snapshots, repository_error, stale};
use crate::bridge::BridgeError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::bridge) struct SearchRequest {
    repo: PathBuf,
    snapshot: String,
    query: String,
    direction: Direction,
    position: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    First,
    Next,
    Previous,
}

impl Snapshots {
    pub(in crate::bridge) fn search(
        &mut self,
        scope: &Scope,
        input: SearchRequest,
    ) -> Result<Value, BridgeError> {
        if input.query.is_empty() || input.query.len() > 512 || input.query.contains('\0') {
            return Err(BridgeError::new("invalid_request", "invalid search query"));
        }
        self.expire();
        let repo = scope.repository(&input.repo)?;
        scope.validate_status_submodules(&repo)?;
        let snapshot = self.current(&repo, &input.snapshot)?;

        let query = input.query.to_lowercase();
        let mut matches = Vec::new();
        for (index, encoded) in snapshot.records.iter().enumerate() {
            let record: Value = serde_json::from_slice(encoded).map_err(repository_error)?;
            let path = record["path"].as_str().ok_or_else(stale)?;
            if path.to_lowercase().contains(&query) {
                matches.push(index);
            }
        }
        let count = matches.len();
        if input.position.is_some_and(|position| position >= count) {
            return Err(BridgeError::new(
                "invalid_request",
                "invalid search position",
            ));
        }
        let position = if count == 0 {
            None
        } else {
            Some(match (input.direction, input.position) {
                (Direction::First, _) | (Direction::Next, None) => 0,
                (Direction::Previous, None) => count - 1,
                (Direction::Next, Some(position)) => (position + 1) % count,
                (Direction::Previous, Some(0)) => count - 1,
                (Direction::Previous, Some(position)) => position - 1,
            })
        };
        let (cursor, page, path, group) = if let Some(position) = position {
            let index = matches[position];
            let page = snapshot
                .page_starts
                .partition_point(|start| *start <= index)
                - 1;
            let record: Value =
                serde_json::from_slice(&snapshot.records[index]).map_err(repository_error)?;
            let path = record["path"].as_str().ok_or_else(stale)?.to_owned();
            let group = record["group"].as_str().ok_or_else(stale)?.to_owned();
            (
                Some(snapshot.page_starts[page]),
                Some(page),
                Some(path),
                Some(group),
            )
        } else {
            (None, None, None, None)
        };
        snapshot.touched = Instant::now();
        Ok(json!({
            "snapshot": input.snapshot,
            "revision": snapshot.revision,
            "count": count,
            "position": position,
            "cursor": cursor,
            "page": page,
            "path": path,
            "group": group,
        }))
    }
}

#[cfg(test)]
mod tests;
