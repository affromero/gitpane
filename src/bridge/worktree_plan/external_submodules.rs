use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::SubmoduleConfig;

use super::super::{BridgeError, scope::Scope, status, submodule::pin};

/// Capture only initialized submodule checkouts whose complete registration
/// chain remains valid throughout the read.
pub(in crate::bridge) fn collect(
    scope: &Scope,
    root: &Path,
    config: &SubmoduleConfig,
) -> Result<Vec<Value>, BridgeError> {
    fn walk(
        scope: &Scope,
        root: &Path,
        parent: &Path,
        config: &SubmoduleConfig,
        chain: &[Value],
        remaining: &mut usize,
    ) -> Result<Vec<Value>, BridgeError> {
        let response = pin::list(scope, parent)?;
        let items = response["submodules"]
            .as_array()
            .ok_or_else(|| BridgeError::new("repository_error", "Invalid submodule listing."))?;
        let mut result = Vec::new();
        for item in items {
            if chain.len() >= 32 {
                return Err(BridgeError::new(
                    "chain_too_large",
                    "Submodule nesting exceeds 32 levels.",
                ));
            }
            if *remaining == 0 {
                return Err(BridgeError::new(
                    "chain_too_large",
                    "Too many worktree submodules.",
                ));
            }
            *remaining -= 1;
            let child = item["repo"]
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| BridgeError::new("repository_error", "Invalid submodule path."))?;
            let mut next = chain.to_vec();
            next.push(
                json!({ "path": item["path"], "parent_identity": item["parent_identity"],
                "child_identity": item["child_identity"] }),
            );
            let proof = || {
                let request = serde_json::from_value(json!({ "root": root, "chain": next }))
                    .map_err(|error| BridgeError::new("invalid_request", error.to_string()))?;
                let proof = pin::validate(scope, request)?;
                if proof["repos"].as_array().and_then(|repos| repos.last()) != Some(&item["repo"]) {
                    return Err(BridgeError::new(
                        "stale_target",
                        "Submodule changed during status collection.",
                    ));
                }
                Ok(())
            };
            proof()?;
            let summary = status::query(&child, scope, config, false, true, &[])?;
            let children = walk(scope, root, &child, config, &next, remaining)?;
            proof()?;
            result.push(json!({ "path": item["path"], "repo": item["repo"],
                "chain": next, "status": summary, "submodules": children }));
        }
        Ok(result)
    }
    walk(scope, root, root, config, &[], &mut 128)
}
