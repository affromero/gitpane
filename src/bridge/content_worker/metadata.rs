use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread::JoinHandle;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{BridgeError, Item, Request, git};

type ObjectInfo = Option<(String, String, u64)>;
type StderrReader = JoinHandle<std::io::Result<(Vec<u8>, bool)>>;

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
// Leave space for the executable, repository arguments and Windows quoting.
const ARGUMENT_UNITS: usize = 8_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: u32,
    oid: String,
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    config: String,
    volatile: bool,
    replacements: String,
    results: Vec<Value>,
}

enum Pending {
    Result(Value),
    Object(Entry, Option<String>),
}

fn failure(message: impl std::fmt::Display) -> BridgeError {
    BridgeError::new("repository_error", message.to_string())
}

fn stale() -> BridgeError {
    BridgeError::new(
        "stale_target",
        "Git content changed; refresh and try again.",
    )
}

fn error(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checked(
    request: &Request,
    args: &[OsString],
    input: Option<&[u8]>,
    remove_config: bool,
) -> Result<Vec<u8>, BridgeError> {
    let output = git(request, args, input, OUTPUT_LIMIT, remove_config)?;
    if !output.success {
        return Err(failure(format!(
            "Git metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn config(request: &Request) -> Result<(String, bool), BridgeError> {
    let result = git(
        request,
        &args(&["config", "--null", "--get-regexp", "textconv$"]),
        None,
        OUTPUT_LIMIT,
        true,
    )?;
    if !(result.success
        || result.code == Some(1) && result.stdout.is_empty() && result.stderr.is_empty())
    {
        return Err(failure(format!(
            "Git configuration lookup failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    if !result.stdout.is_empty() && !result.stdout.ends_with(b"\0") {
        return Err(failure("Malformed Git configuration output."));
    }
    Ok((digest(&result.stdout), !result.stdout.is_empty()))
}

fn replacements(request: &Request) -> Result<(String, bool), BridgeError> {
    let base = std::env::var_os("GIT_REPLACE_REF_BASE").unwrap_or_else(|| "refs/replace".into());
    let mut arguments = args(&["for-each-ref", "--format=%(refname) %(objectname)", "--"]);
    arguments.push(base);
    let output = checked(request, &arguments, None, false)?;
    Ok((digest(&output), !output.is_empty()))
}

fn head(request: &Request) -> Result<Option<String>, BridgeError> {
    let output = git(
        request,
        &args(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
        None,
        OUTPUT_LIMIT,
        false,
    )?;
    if !output.success {
        if output.code == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        return Err(failure(format!(
            "Git HEAD lookup failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let value = std::str::from_utf8(&output.stdout)
        .map_err(failure)?
        .trim_end_matches('\n');
    if !super::super::changes::blob::commit_oid(value) {
        return Err(failure("Malformed Git HEAD object ID."));
    }
    Ok(Some(value.to_owned()))
}

fn oid(value: &[u8]) -> Result<String, BridgeError> {
    let value = std::str::from_utf8(value).map_err(failure)?;
    if !super::super::changes::blob::commit_oid(value) {
        return Err(failure("Malformed Git object ID."));
    }
    Ok(value.to_owned())
}

fn records(output: &[u8]) -> Result<impl Iterator<Item = &[u8]>, BridgeError> {
    if !output.is_empty() && !output.ends_with(b"\0") {
        return Err(failure("Truncated Git metadata output."));
    }
    Ok(output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty()))
}

fn parse_entry(record: &[u8], index: bool) -> Result<(String, Option<u8>, Entry), BridgeError> {
    let separator = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| failure("Malformed Git entry."))?;
    let (metadata, path) = (&record[..separator], &record[separator + 1..]);
    let mut fields = metadata.split(|byte| *byte == b' ');
    let mode = fields
        .next()
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| u32::from_str_radix(value, 8).ok())
        .ok_or_else(|| failure("Malformed Git file mode."))?;
    let second = fields
        .next()
        .ok_or_else(|| failure("Malformed Git entry."))?;
    let third = fields
        .next()
        .ok_or_else(|| failure("Malformed Git entry."))?;
    if fields.next().is_some() {
        return Err(failure("Malformed Git entry."));
    }
    let (object, stage) = if index {
        let stage = match third {
            b"0" => 0,
            b"1" => 1,
            b"2" => 2,
            b"3" => 3,
            _ => return Err(failure("Malformed Git index stage.")),
        };
        (second, Some(stage))
    } else {
        if !matches!(second, b"blob" | b"tree" | b"commit") {
            return Err(failure("Malformed Git tree entry."));
        }
        (third, None)
    };
    let path = std::str::from_utf8(path).map_err(failure)?.to_owned();
    Ok((
        path,
        stage,
        Entry {
            mode,
            oid: oid(object)?,
        },
    ))
}

fn chunks(paths: &BTreeSet<String>) -> Result<Vec<Vec<String>>, BridgeError> {
    let mut result: Vec<Vec<String>> = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;
    for path in paths {
        // Windows quoting can double every backslash, plus quotes and space.
        let size = path
            .encode_utf16()
            .count()
            .saturating_mul(2)
            .saturating_add(3);
        if size > ARGUMENT_UNITS {
            return Err(failure(
                "Git path exceeds the supported command line limit.",
            ));
        }
        if used + size > ARGUMENT_UNITS {
            result.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(path.clone());
        used += size;
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}

fn entries(
    request: &Request,
    paths: &BTreeSet<String>,
    reference: Option<&str>,
) -> Result<BTreeMap<(String, Option<u8>), Entry>, BridgeError> {
    let mut entries = BTreeMap::new();
    for chunk in chunks(paths)? {
        let mut command = if let Some(reference) = reference {
            vec![
                "--literal-pathspecs".into(),
                "ls-tree".into(),
                "-z".into(),
                reference.into(),
                "--".into(),
            ]
        } else {
            args(&["--literal-pathspecs", "ls-files", "--stage", "-z", "--"])
        };
        command.extend(chunk.iter().map(OsString::from));
        let output = checked(request, &command, None, false)?;
        for record in records(&output)? {
            let (path, stage, entry) = parse_entry(record, reference.is_none())?;
            if paths.contains(&path) && entries.insert((path, stage), entry).is_some() {
                return Err(failure("Duplicate Git metadata entry."));
            }
        }
    }
    Ok(entries)
}

fn batch_objects(request: &Request, specs: &[String]) -> Result<Vec<ObjectInfo>, BridgeError> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    for spec in specs {
        if !super::super::changes::blob::commit_oid(spec) {
            return Err(failure("Invalid Git object ID."));
        }
        input.extend_from_slice(spec.as_bytes());
        input.push(b'\n');
    }
    let output = checked(
        request,
        &args(&[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ]),
        Some(&input),
        false,
    )?;
    let mut rest = output.as_slice();
    let mut result = Vec::with_capacity(specs.len());
    for spec in specs {
        let mut missing = spec.as_bytes().to_vec();
        missing.extend_from_slice(b" missing\n");
        if rest.starts_with(&missing) {
            rest = &rest[missing.len()..];
            result.push(None);
            continue;
        }
        let end = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| failure("Truncated Git object output."))?;
        let mut fields = rest[..end].split(|byte| *byte == b' ');
        let id = oid(fields
            .next()
            .ok_or_else(|| failure("Malformed Git object output."))?)?;
        let kind = std::str::from_utf8(
            fields
                .next()
                .ok_or_else(|| failure("Malformed Git object output."))?,
        )
        .map_err(failure)?
        .to_owned();
        let size = fields
            .next()
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| failure("Malformed Git object size."))?;
        if fields.next().is_some() || !matches!(kind.as_str(), "blob" | "tree" | "commit" | "tag") {
            return Err(failure("Malformed Git object output."));
        }
        result.push(Some((id, kind, size)));
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() {
        return Err(failure("Unexpected Git object output."));
    }
    Ok(result)
}

fn canonical_results(request: &Request, items: &[Item]) -> Result<Vec<Value>, BridgeError> {
    let head = if items.iter().any(|item| item.reference == "HEAD") {
        head(request)?
    } else {
        None
    };
    let mut index_paths = BTreeSet::new();
    let mut tree_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in items {
        if item.reference.starts_with(':') {
            index_paths.insert(item.path.clone());
        } else if let Some(reference) = if item.reference == "HEAD" {
            head.as_ref()
        } else {
            Some(&item.reference)
        } {
            tree_paths
                .entry(reference.clone())
                .or_default()
                .insert(item.path.clone());
        }
    }
    let index = entries(request, &index_paths, None)?;
    let mut trees = BTreeMap::new();
    let references = tree_paths.keys().cloned().collect::<Vec<_>>();
    let mut reference_errors = BTreeMap::new();
    for (reference, object) in references.iter().zip(batch_objects(request, &references)?) {
        match object {
            None => {
                reference_errors.insert(
                    reference.clone(),
                    error("content_not_found", "Git content was not found."),
                );
            }
            Some((oid, kind, _)) if oid == *reference && kind == "commit" => {}
            Some((oid, _, _)) if oid == *reference => {
                reference_errors.insert(
                    reference.clone(),
                    error(
                        "unsupported_content",
                        "Git content reference is not a commit.",
                    ),
                );
            }
            _ => return Err(stale()),
        }
    }
    for (reference, paths) in tree_paths {
        if !reference_errors.contains_key(&reference) {
            trees.insert(
                reference.clone(),
                entries(request, &paths, Some(&reference))?,
            );
        }
    }
    let mut specs = Vec::new();
    let mut requests = Vec::new();
    for item in items {
        let reference = if item.reference == "HEAD" {
            head.as_deref()
        } else {
            Some(item.reference.as_str())
        };
        let Some(reference) = reference else {
            requests.push(Pending::Result(error(
                "content_not_found",
                "Git content was not found.",
            )));
            continue;
        };
        if let Some(error) = reference_errors.get(reference) {
            requests.push(Pending::Result(error.clone()));
            continue;
        }
        let entry = if reference.starts_with(':') {
            let stage = reference.as_bytes()[1] - b'0';
            index.get(&(item.path.clone(), Some(stage)))
        } else {
            trees
                .get(reference)
                .and_then(|tree| tree.get(&(item.path.clone(), None)))
        };
        let Some(entry) = entry else {
            requests.push(Pending::Result(error(
                "content_not_found",
                "Git content was not found.",
            )));
            continue;
        };
        if !matches!(entry.mode & 0o170000, 0o100000 | 0o120000) {
            requests.push(Pending::Result(error(
                "unsupported_content",
                "Git content is not a file or symlink.",
            )));
            continue;
        }
        specs.push(entry.oid.clone());
        requests.push(Pending::Object(
            entry.clone(),
            if reference.starts_with(':') {
                None
            } else {
                Some(reference.to_owned())
            },
        ));
    }
    let mut objects = batch_objects(request, &specs)?.into_iter();
    let mut results = Vec::with_capacity(items.len());
    for item in requests {
        let (entry, head_oid) = match item {
            Pending::Result(result) => {
                results.push(result);
                continue;
            }
            Pending::Object(entry, head_oid) => (entry, head_oid),
        };
        let actual = objects
            .next()
            .ok_or_else(|| failure("Missing Git object result."))?;
        match actual {
            Some((oid, kind, size)) if oid == entry.oid => {
                if kind != "blob" {
                    results.push(error(
                        "unsupported_content",
                        "Git content is not a file or symlink.",
                    ));
                } else {
                    results.push(json!({"metadata": {"oid": oid, "mode": entry.mode, "size": size, "head_oid": head_oid}}));
                }
            }
            _ => return Err(stale()),
        }
    }
    Ok(results)
}

const MODE_FORMAT: &str = "--batch-check=%(objectname) %(objectmode) %(objecttype) %(objectsize)";

fn supports_object_mode(request: &Request) -> Result<bool, BridgeError> {
    let output = super::runner::git_probe(
        request,
        &args(&["cat-file", MODE_FORMAT, "-Z"]),
        Some(b""),
        4096,
    )?;
    if output.success {
        if !output.stdout.is_empty() {
            return Err(failure("Unexpected Git capability output."));
        }
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let unsupported = stderr.lines().any(|line| {
        line == "fatal: unknown format element: objectmode"
            || line == "fatal: bad cat-file format: %(objectmode)"
            || line == "error: unknown switch `Z'"
            || line == "error: unknown option `Z'"
    });
    if unsupported {
        return Ok(false);
    }
    Err(failure(format!(
        "Git capability check failed: {}",
        stderr.trim()
    )))
}

struct CatFile {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    stderr: Option<StderrReader>,
    total: usize,
}

impl CatFile {
    fn new(request: &Request) -> Result<Self, BridgeError> {
        let mut child = super::runner::command(request)
            .args(["cat-file", MODE_FORMAT, "-Z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(failure)?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| failure("Git input is unavailable."))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("Git output is unavailable."))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| failure("Git error output is unavailable."))?;
        let reader = std::thread::spawn(move || {
            let mut result = Vec::new();
            let mut overflow = false;
            let mut buffer = [0u8; 8192];
            loop {
                let count = stderr.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let take = count.min(65536usize.saturating_sub(result.len()));
                result.extend_from_slice(&buffer[..take]);
                overflow |= count != take;
            }
            Ok((result, overflow))
        });
        Ok(Self {
            child: Some(child),
            input: Some(input),
            output: BufReader::new(output),
            stderr: Some(reader),
            total: 0,
        })
    }

    fn query(&mut self, spec: &str) -> Result<ModeObject, BridgeError> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| failure("Git input is unavailable."))?;
        input
            .write_all(spec.as_bytes())
            .and_then(|_| input.write_all(b"\0"))
            .and_then(|_| input.flush())
            .map_err(failure)?;
        let mut record = Vec::new();
        (&mut self.output)
            .take((OUTPUT_LIMIT + 1) as u64)
            .read_until(0, &mut record)
            .map_err(failure)?;
        self.total = self.total.saturating_add(record.len());
        if self.total > OUTPUT_LIMIT || record.last() != Some(&0) {
            return Err(failure("Truncated or oversized Git object output."));
        }
        record.pop();
        if record == [spec.as_bytes(), b" missing"].concat() {
            return Ok(ModeObject::Missing);
        }
        if let Some(object) = record.strip_suffix(b" submodule") {
            oid(object)?;
            return Ok(ModeObject::Submodule);
        }
        let mut fields = record.split(|byte| *byte == b' ');
        let object = oid(fields
            .next()
            .ok_or_else(|| failure("Malformed Git object output."))?)?;
        let mode = fields
            .next()
            .ok_or_else(|| failure("Malformed Git object output."))?;
        let mode = if mode.is_empty() {
            None
        } else {
            Some(
                u32::from_str_radix(std::str::from_utf8(mode).map_err(failure)?, 8)
                    .map_err(failure)?,
            )
        };
        let kind = std::str::from_utf8(
            fields
                .next()
                .ok_or_else(|| failure("Malformed Git object output."))?,
        )
        .map_err(failure)?
        .to_owned();
        let size = fields
            .next()
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| failure("Malformed Git object size."))?;
        if fields.next().is_some() || !matches!(kind.as_str(), "blob" | "tree" | "commit" | "tag") {
            return Err(failure("Malformed Git object output."));
        }
        Ok(ModeObject::Present {
            oid: object,
            mode,
            kind,
            size,
        })
    }

    fn finish(mut self) -> Result<(), BridgeError> {
        drop(self.input.take());
        let mut extra = [0u8; 1];
        if self.output.read(&mut extra).map_err(failure)? != 0 {
            return Err(failure("Unexpected Git object output."));
        }
        let status = self
            .child
            .as_mut()
            .ok_or_else(|| failure("Git process is unavailable."))?
            .wait()
            .map_err(failure)?;
        self.child.take();
        let (stderr, overflow) = self
            .stderr
            .take()
            .ok_or_else(|| failure("Git error reader is unavailable."))?
            .join()
            .map_err(|_| failure("Git error reader failed."))?
            .map_err(failure)?;
        if overflow {
            return Err(failure("Git error output exceeds the size limit."));
        }
        if !status.success() {
            return Err(failure(format!(
                "Git metadata failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok(())
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        drop(self.input.take());
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

enum ModeObject {
    Missing,
    Submodule,
    Present {
        oid: String,
        mode: Option<u32>,
        kind: String,
        size: u64,
    },
}

fn mode_results(request: &Request, items: &[Item]) -> Result<Vec<Value>, BridgeError> {
    let mut cat = CatFile::new(request)?;
    let head = if items.iter().any(|item| item.reference == "HEAD") {
        match cat.query("HEAD^{commit}")? {
            ModeObject::Missing => None,
            ModeObject::Present {
                oid,
                mode: None,
                kind,
                ..
            } if kind == "commit" => Some(oid),
            _ => return Err(failure("Malformed Git HEAD object output.")),
        }
    } else {
        None
    };
    let references = items
        .iter()
        .filter(|item| super::super::changes::blob::commit_oid(&item.reference))
        .map(|item| item.reference.clone())
        .collect::<BTreeSet<_>>();
    let mut reference_errors = BTreeMap::new();
    for reference in references {
        match cat.query(&reference)? {
            ModeObject::Missing => {
                reference_errors.insert(
                    reference,
                    error("content_not_found", "Git content was not found."),
                );
            }
            ModeObject::Present {
                oid,
                mode: None,
                kind,
                ..
            } if oid == reference && kind == "commit" => {}
            ModeObject::Present {
                oid, mode: None, ..
            } if oid == reference => {
                reference_errors.insert(
                    reference,
                    error(
                        "unsupported_content",
                        "Git content reference is not a commit.",
                    ),
                );
            }
            _ => return Err(stale()),
        }
    }
    let mut results = Vec::with_capacity(items.len());
    let mut missing: BTreeMap<Option<String>, BTreeSet<String>> = BTreeMap::new();
    let mut missing_specs = Vec::new();
    for item in items {
        let reference = if item.reference == "HEAD" {
            head.as_deref()
        } else {
            Some(item.reference.as_str())
        };
        let Some(reference) = reference else {
            results.push(error("content_not_found", "Git content was not found."));
            continue;
        };
        if let Some(error) = reference_errors.get(reference) {
            results.push(error.clone());
            continue;
        }
        match cat.query(&format!("{reference}:{}", item.path))? {
            ModeObject::Missing => {
                let key = if reference.starts_with(':') {
                    None
                } else {
                    Some(reference.to_owned())
                };
                let stage = if reference.starts_with(':') {
                    Some(reference.as_bytes()[1] - b'0')
                } else {
                    None
                };
                missing
                    .entry(key.clone())
                    .or_default()
                    .insert(item.path.clone());
                missing_specs.push((key, item.path.clone(), stage));
                results.push(error("content_not_found", "Git content was not found."));
            }
            ModeObject::Submodule => results.push(error(
                "unsupported_content",
                "Git content is not a file or symlink.",
            )),
            ModeObject::Present {
                oid,
                mode: Some(mode),
                kind,
                size,
            } => {
                if !matches!(mode & 0o170000, 0o100000 | 0o120000) || kind != "blob" {
                    results.push(error(
                        "unsupported_content",
                        "Git content is not a file or symlink.",
                    ));
                } else {
                    let head_oid = if reference.starts_with(':') {
                        None
                    } else {
                        Some(reference)
                    };
                    results.push(json!({"metadata": {"oid": oid, "mode": mode, "size": size, "head_oid": head_oid}}));
                }
            }
            _ => return Err(failure("Malformed Git file mode.")),
        }
    }
    cat.finish()?;
    for (reference, paths) in missing {
        let found = entries(request, &paths, reference.as_deref())?;
        for (missing_ref, path, stage) in &missing_specs {
            if *missing_ref == reference && found.contains_key(&(path.clone(), *stage)) {
                return Err(stale());
            }
        }
    }
    Ok(results)
}

fn snapshot(request: &Request, items: &[Item], fast: bool) -> Result<Snapshot, BridgeError> {
    let (config, config_volatile) = config(request)?;
    let (replacements, replacements_volatile) = replacements(request)?;
    let results = if fast {
        mode_results(request, items)?
    } else {
        canonical_results(request, items)?
    };
    Ok(Snapshot {
        config,
        volatile: config_volatile || replacements_volatile,
        replacements,
        results,
    })
}

pub(super) fn metadata(request: &Request, items: &[Item]) -> Result<Value, BridgeError> {
    let fast = supports_object_mode(request)?;
    let first = snapshot(request, items, fast)?;
    let second = snapshot(request, items, fast)?;
    if first != second {
        return Err(stale());
    }
    Ok(
        json!({"version": 1, "config": first.config, "volatile": first.volatile, "replacements": first.replacements, "results": first.results}),
    )
}

pub(super) fn verified_content(
    request: &Request,
    reference: &str,
    path: &str,
) -> Result<(), BridgeError> {
    let fast = supports_object_mode(request)?;
    let items = [Item {
        reference: reference.to_owned(),
        path: path.to_owned(),
    }];
    let first = snapshot(request, &items, fast)?;
    let result = first
        .results
        .first()
        .ok_or_else(|| failure("Git metadata is unavailable."))?;
    if let Some(error) = result.get("error") {
        let code = match error.get("code").and_then(Value::as_str) {
            Some("content_not_found") => "content_not_found",
            Some("unsupported_content") => "unsupported_content",
            _ => return Err(failure("Malformed Git metadata error.")),
        };
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("Malformed Git metadata error."))?;
        return Err(BridgeError::new(code, message));
    }
    let pinned = if reference == "HEAD" {
        let head = result
            .get("metadata")
            .and_then(|value| value.get("head_oid"))
            .and_then(Value::as_str)
            .ok_or_else(|| failure("Git HEAD is unavailable."))?;
        if !super::super::changes::blob::commit_oid(head) {
            return Err(failure("Malformed Git HEAD object ID."));
        }
        head
    } else {
        reference
    };
    super::runner::content_drained(request, pinned, path)?;
    if first != snapshot(request, &items, fast)? {
        return Err(stale());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
