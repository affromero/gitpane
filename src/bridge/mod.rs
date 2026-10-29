use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Config, GraphConfig, SubmoduleConfig, WorktreeConfig};

mod changes;
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
mod theme;
mod worktree_plan;

use scope::Scope;

const PROTOCOL_VERSION: u64 = 1;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
// Leave room below the extension host's 16 MiB line limit for framing.
const MAX_RESPONSE_BYTES: usize = 15 * 1024 * 1024;

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
    #[serde(default)]
    workspace_root_identities: Option<Vec<String>>,
    #[serde(default)]
    workspace_root_sources: Option<Vec<PathBuf>>,
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
    #[serde(default)]
    summary_only: bool,
    #[serde(default)]
    watched_paths: Vec<PathBuf>,
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

struct BridgeOptions<'a> {
    submodules: &'a SubmoduleConfig,
    graph: &'a GraphConfig,
    worktree: &'a WorktreeConfig,
    review_base: Option<&'a str>,
    theme: &'a Config,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}

fn process(
    scope: &mut Option<Scope>,
    snapshots: &mut changes::Snapshots,
    request: Request,
    options: &BridgeOptions<'_>,
) -> Result<Value, BridgeError> {
    if request.method == "initialize" {
        let init: Initialize = params(request.params)?;
        if init.version != PROTOCOL_VERSION {
            return Err(BridgeError::new(
                "unsupported_version",
                "unsupported bridge protocol version",
            ));
        }
        let updated =
            if init.workspace_root_identities.is_none() && init.workspace_root_sources.is_none() {
                Scope::new(init.workspace_roots)?
            } else {
                Scope::new_with_identities(
                    init.workspace_roots,
                    init.workspace_root_identities,
                    init.workspace_root_sources,
                )?
            };
        snapshots.clear();
        *scope = Some(updated);
        return Ok(json!({ "version": PROTOCOL_VERSION }));
    }
    let scope = scope.as_ref().ok_or_else(|| {
        BridgeError::new(
            "not_initialized",
            "initialize the bridge before making requests",
        )
    })?;
    let sub_cfg = options.submodules;
    let graph_cfg = options.graph;
    let worktree_cfg = options.worktree;
    let review_base = options.review_base;
    match request.method.as_str() {
        "workspace_root_identities" => {
            let _: EmptyRequest = params(request.params)?;
            scope.identities()
        }
        "theme" => {
            let _: EmptyRequest = params(request.params)?;
            theme::query(options.theme)
        }
        "changes_begin" => snapshots.begin(scope, params(request.params)?, sub_cfg),
        "changes_next" => snapshots.page(scope, params(request.params)?),
        "changes_find" => snapshots.find(scope, params(request.params)?),
        "changes_release" => snapshots.release(scope, params(request.params)?),
        "file_action_preview" => {
            let input = params(request.params)?;
            mutation::file_action_preview(scope, input)
        }
        "working_symlink" => mutation::working_symlink(scope, params(request.params)?),
        "worktree_remove_preview" => {
            mutation::worktree_remove_preview(scope, params(request.params)?)
        }
        "status" => {
            let input: StatusRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let result = status::query(
                &repo,
                scope,
                sub_cfg,
                input.include_worktrees,
                input.summary_only,
                &input.watched_paths,
            )?;
            scope.repository(&repo)?;
            Ok(result)
        }
        "submodule_inspect" => submodule::inspect(scope, params(request.params)?, sub_cfg),
        "submodule_chain_validate" => {
            submodule::chain::validate(scope, params(request.params)?, sub_cfg)
        }
        "sessions_list" => session::list(scope, params(request.params)?),
        "session_liveness" => session::liveness(scope, params(request.params)?, options.theme),
        "launchers_list" => launcher::list(scope, params(request.params)?),
        "launcher_plan" => launcher::plan(scope, params(request.params)?),
        "launcher_validate" => launcher::validate(scope, params(request.params)?),
        "worktree_plan" => worktree_plan::plan(scope, params(request.params)?, worktree_cfg),
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
                graph_cfg,
                input.first_parent,
                input.filters,
                input.collapsed_segments,
            )?;
            scope.repository(&repo)?;
            Ok(result)
        }
        "graph_revision" => {
            let input: RepositoryRequest = params(request.params)?;
            let repo = scope.repository(&input.repo)?;
            let result = graph::revision(&repo, scope, graph_cfg)?;
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
    let encoded = serde_json::to_vec(&reply).map_err(io::Error::other)?;
    if encoded.len() > MAX_RESPONSE_BYTES {
        let error = json!({ "id": id, "error": {
            "code": "response_too_large",
            "message": "Gitpane response exceeds the supported size limit"
        }});
        serde_json::to_writer(&mut *writer, &error).map_err(io::Error::other)?;
    } else {
        writer.write_all(&encoded)?;
    }
    writer.write_all(b"\n")?;
    writer.flush()
}

fn serve_inner(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    options: &BridgeOptions<'_>,
    mutation_once: bool,
    liveness_worker: bool,
) -> io::Result<()> {
    let mut scope = None;
    let mut snapshots = changes::Snapshots::default();
    loop {
        if liveness_worker && crate::session::liveness::worker_cancelled() {
            return Ok(());
        }
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
                let liveness_request = liveness_worker && request.method == "session_liveness";
                let liveness_initialization = liveness_worker && request.method == "initialize";
                if liveness_worker
                    && !matches!(request.method.as_str(), "initialize" | "session_liveness")
                {
                    write_reply(
                        writer,
                        Some(id),
                        Err(BridgeError::new(
                            "invalid_request",
                            "liveness worker only accepts session liveness requests",
                        )),
                    )?;
                    continue;
                }
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
                let mut result = process(&mut scope, &mut snapshots, request, options);
                if liveness_initialization && let Ok(value) = &mut result {
                    value["capabilities"] = json!({"liveness_worker": true});
                }
                let failed = result.is_err();
                write_reply(writer, Some(id), result)?;
                if liveness_request {
                    return Ok(());
                }
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

#[cfg(test)]
fn serve(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    sub_cfg: &SubmoduleConfig,
    graph_cfg: &GraphConfig,
    worktree_cfg: &WorktreeConfig,
    review_base: Option<&str>,
    mutation_once: bool,
) -> io::Result<()> {
    serve_inner(
        reader,
        writer,
        &BridgeOptions {
            submodules: sub_cfg,
            graph: graph_cfg,
            worktree: worktree_cfg,
            review_base,
            theme: &Config::default(),
        },
        mutation_once,
        false,
    )
}

pub(crate) fn run(
    config: &Config,
    mutation_once: bool,
    liveness_worker: bool,
) -> color_eyre::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_inner(
        &mut stdin.lock(),
        &mut stdout.lock(),
        &BridgeOptions {
            submodules: &config.submodules,
            graph: &config.graph,
            worktree: &config.worktree,
            review_base: config.review.base.as_deref(),
            theme: config,
        },
        mutation_once,
        liveness_worker,
    )?;
    Ok(())
}
