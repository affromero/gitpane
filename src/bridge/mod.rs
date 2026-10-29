use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::SubmoduleConfig;

mod file_identity;
mod github;
mod graph;
mod launcher;
mod mutation;
#[cfg(test)]
mod mutation_tests;
mod repo_sort;
mod scope;
mod session;
mod status;
mod submodule;
#[cfg(test)]
mod tests;

use scope::Scope;

const PROTOCOL_VERSION: u64 = 1;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
struct BridgeError {
    code: &'static str,
    message: String,
}

impl BridgeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Initialize {
    version: u64,
    workspace_roots: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryRequest {
    repo: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    repo: PathBuf,
    #[serde(default = "include_worktrees_by_default")]
    include_worktrees: bool,
}

fn include_worktrees_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitFilesRequest {
    repo: PathBuf,
    oid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphRequest {
    repo: PathBuf,
    #[serde(default)]
    first_parent: bool,
    #[serde(default)]
    filters: graph::FilterRequest,
    #[serde(default)]
    collapsed_segments: std::collections::BTreeSet<String>,
}

fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, BridgeError> {
    serde_json::from_value(value)
        .map_err(|error| BridgeError::new("invalid_request", error.to_string()))
}

fn commit_file_json(file: crate::git::commit_files::CommitFile) -> Value {
    let mut result =
        json!({"status": file.status, "path": file.path, "is_submodule": file.is_submodule});
    if let Some(old_path) = file.old_path {
        result["old_path"] = json!(old_path);
    }
    result
}

fn process(
    scope: &mut Option<Scope>,
    request: Request,
    sub_cfg: &SubmoduleConfig,
    review_base: Option<&str>,
) -> Result<Value, BridgeError> {
    if request.method == "initialize" {
        let init: Initialize = params(request.params)?;
        if init.version != PROTOCOL_VERSION {
            return Err(BridgeError::new(
                "unsupported_version",
                "unsupported bridge protocol version",
            ));
        }
        *scope = Some(Scope::new(init.workspace_roots)?);
        return Ok(json!({ "version": PROTOCOL_VERSION }));
    }
    let scope = scope.as_ref().ok_or_else(|| {
        BridgeError::new(
            "not_initialized",
            "initialize the bridge before making requests",
        )
    })?;
    match request.method.as_str() {
        "file_action_preview" => {
            let input = params(request.params)?;
            mutation::file_action_preview(scope, input)
        }
        "status" => {
            let input: StatusRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let result = status::query(&repo, scope, sub_cfg, input.include_worktrees)?;
            scope.repository(&repo)?;
            Ok(result)
        }
        "submodule_inspect" => submodule::inspect(scope, params(request.params)?, sub_cfg),
        "sessions_list" => session::list(scope, params(request.params)?),
        "launchers_list" => launcher::list(scope, params(request.params)?),
        "launcher_plan" => launcher::plan(scope, params(request.params)?),
        "launcher_validate" => launcher::validate(scope, params(request.params)?),
        "commit_files" => {
            let input: CommitFilesRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let (message, files) =
                crate::git::commit_files::list_commit_files_detailed(&repo, &input.oid)
                    .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
            scope.repository(&repo)?;
            Ok(json!({
                "message": message,
                "files": files.into_iter().map(commit_file_json).collect::<Vec<_>>(),
            }))
        }
        "branch_review" => {
            let input: RepositoryRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let (base_ref, base_oid, head_oid, files) =
                crate::git::commit_files::list_review_files(&repo, review_base)
                    .map_err(|error| BridgeError::new("repository_error", error.to_string()))?;
            scope.repository(&repo)?;
            Ok(json!({
                "base_ref": base_ref, "base_oid": base_oid.to_string(), "head_oid": head_oid.to_string(),
                "files": files.into_iter().map(commit_file_json).collect::<Vec<_>>(),
            }))
        }
        "graph" => {
            let input: GraphRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let result = graph::query(
                &repo,
                scope,
                input.first_parent,
                input.filters,
                input.collapsed_segments,
            )?;
            scope.repository(&repo)?;
            Ok(result)
        }
        "github_list" => github::list(scope, params(request.params)?),
        "github_detail" => github::detail(scope, params(request.params)?),
        "sort_repositories" => repo_sort::sort(scope, params(request.params)?),
        _ => Err(BridgeError::new("invalid_request", "unknown bridge method")),
    }
}

fn process_mutation(scope: &Scope, request: Request) -> Result<Value, BridgeError> {
    match request.method.as_str() {
        "file_action" => mutation::file_action(scope, params(request.params)?),
        "repo_sync" => mutation::repo_sync(scope, params(request.params)?),
        "session_goto" => session::goto(scope, params(request.params)?),
        "submodule_action" => submodule::actions::execute(scope, params(request.params)?),
        "worktree_create" => mutation::worktree_create(scope, params(request.params)?),
        "worktree_remove" => mutation::worktree_remove(scope, params(request.params)?),
        _ => Err(BridgeError::new(
            "invalid_request",
            "expected a mutation request",
        )),
    }
}

fn write_reply(
    writer: &mut impl Write,
    id: Option<u64>,
    result: Result<Value, BridgeError>,
) -> io::Result<()> {
    let reply = match result {
        Ok(value) => json!({ "id": id, "result": value }),
        Err(error) => {
            json!({ "id": id, "error": { "code": error.code, "message": error.message } })
        }
    };
    serde_json::to_writer(&mut *writer, &reply).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn serve(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    sub_cfg: &SubmoduleConfig,
    review_base: Option<&str>,
    mutation_once: bool,
) -> io::Result<()> {
    let mut scope = None;
    loop {
        let mut line = Vec::new();
        let count = (&mut *reader)
            .take(MAX_REQUEST_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            return Ok(());
        }
        if line.len() as u64 > MAX_REQUEST_BYTES {
            write_reply(
                writer,
                None,
                Err(BridgeError::new(
                    "request_too_large",
                    "request exceeds the size limit",
                )),
            )?;
            return Ok(());
        }
        match serde_json::from_slice::<Request>(&line) {
            Ok(request) => {
                let id = request.id;
                if mutation_once && let Some(scope) = scope.as_ref() {
                    let result = process_mutation(scope, request);
                    write_reply(writer, Some(id), result)?;
                    return Ok(());
                }
                if mutation_once && request.method != "initialize" {
                    write_reply(
                        writer,
                        Some(id),
                        Err(BridgeError::new(
                            "not_initialized",
                            "initialize the bridge before making requests",
                        )),
                    )?;
                    return Ok(());
                }
                let result = process(&mut scope, request, sub_cfg, review_base);
                let failed = result.is_err();
                write_reply(writer, Some(id), result)?;
                if mutation_once && failed {
                    return Ok(());
                }
            }
            Err(error) => {
                write_reply(
                    writer,
                    None,
                    Err(BridgeError::new("invalid_request", error.to_string())),
                )?;
                if mutation_once {
                    return Ok(());
                }
            }
        }
    }
}

pub(crate) fn run(
    sub_cfg: &SubmoduleConfig,
    review_base: Option<&str>,
    mutation_once: bool,
) -> color_eyre::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(
        &mut stdin.lock(),
        &mut stdout.lock(),
        sub_cfg,
        review_base,
        mutation_once,
    )?;
    Ok(())
}
