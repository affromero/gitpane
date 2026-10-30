use crate::bridge::BridgeError;

pub(super) const MAX_CONTENT: usize = 1024 * 1024;

/// Read an object from this repository without selecting a repository by path.
pub(super) fn read_text(repo: &git2::Repository, oid: git2::Oid) -> Result<String, BridgeError> {
    let error = |error: git2::Error| BridgeError::new("repository_error", error.to_string());
    let invalid = |message| BridgeError::new("invalid_request", message);
    let (size, kind) = repo.odb().map_err(error)?.read_header(oid).map_err(error)?;
    if kind != git2::ObjectType::Blob {
        return Err(BridgeError::new(
            "repository_error",
            "Pointer side is not a blob.",
        ));
    }
    if size > MAX_CONTENT {
        return Err(invalid("Pointer side exceeds the 1 MiB text limit."));
    }
    let blob = repo.find_blob(oid).map_err(error)?;
    if blob.size() > MAX_CONTENT {
        return Err(invalid("Pointer side exceeds the 1 MiB text limit."));
    }
    let bytes = blob.content();
    if bytes.contains(&0) {
        return Err(invalid("Binary pointer sides cannot be opened as text."));
    }
    std::str::from_utf8(bytes)
        .map_err(|_| invalid("Pointer side is not UTF-8 text."))
        .map(str::to_owned)
}
