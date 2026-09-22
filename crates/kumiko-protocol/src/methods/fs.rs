//! File system access through the core (permission `fs.workspace`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use kumiko_types::Uri;

use crate::common::Content;
use crate::methods::document::DocumentParams;
use crate::methods::event::SubscribeResult;
use crate::request;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsReadResult {
    pub content: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsWriteParams {
    pub uri: Uri,
    pub content: Content,
    /// Create the file if it does not exist.
    pub create: bool,
    /// Replace an existing file.
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub kind: FileKind,
    pub size: u64,
    /// Modification time, milliseconds since the Unix epoch.
    pub mtime_ms: Option<u64>,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsReadDirResult {
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsDeleteParams {
    pub uri: Uri,
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsRenameParams {
    pub from: Uri,
    pub to: Uri,
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsWatchParams {
    /// Glob relative to `base`, e.g. `**/*.toml`.
    pub glob: String,
    /// Directory the glob is relative to; all workspace folders when omitted.
    pub base: Option<Uri>,
}

request!(
    FsRead,
    "fs/read",
    PluginToHost,
    DocumentParams,
    FsReadResult,
    "Read a whole file."
);
request!(
    FsWrite,
    "fs/write",
    PluginToHost,
    FsWriteParams,
    (),
    "Write a whole file."
);
request!(
    FsStat,
    "fs/stat",
    PluginToHost,
    DocumentParams,
    FileStat,
    "File metadata."
);
request!(
    FsReadDir,
    "fs/readDir",
    PluginToHost,
    DocumentParams,
    FsReadDirResult,
    "Directory entries."
);
request!(
    FsDelete,
    "fs/delete",
    PluginToHost,
    FsDeleteParams,
    (),
    "Delete a file or directory."
);
request!(
    FsRename,
    "fs/rename",
    PluginToHost,
    FsRenameParams,
    (),
    "Rename or move a file or directory."
);
request!(
    FsWatch,
    "fs/watch",
    PluginToHost,
    FsWatchParams,
    SubscribeResult,
    "Watch files; changes arrive as `fs.changed` events of the returned subscription."
);
