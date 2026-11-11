use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StashSource {
    Tracked,
    SavedUntracked,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StashFile {
    pub source: StashSource,
    pub status: String,
    /// The new path, or the old path for a deletion. Together with source, this is the file key.
    pub path: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub is_binary: bool,
    pub is_submodule: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StashFiles {
    pub oid: String,
    pub message: String,
    pub parent_oid: String,
    pub untracked_parent_oid: Option<String>,
    pub files: Vec<StashFile>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StashContentKind {
    Missing,
    Text,
    Binary,
    Gitlink,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StashContent {
    pub kind: StashContentKind,
    pub oid: Option<String>,
    /// Git tree entry mode (for example, 0o100644 or 0o160000).
    pub mode: Option<u32>,
    pub size: usize,
    /// Missing sides contain empty text. Binary sides never contain decoded text.
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StashFileContent {
    pub oid: String,
    pub file: StashFile,
    pub old: StashContent,
    pub new: StashContent,
    pub diff: String,
}
