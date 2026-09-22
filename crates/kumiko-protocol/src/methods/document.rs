//! Documents owned by the core Document Service (spec, section 12).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use kumiko_types::{DocumentVersion, TextEdit, Uri};

use crate::request;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentParams {
    pub uri: Uri,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentText {
    pub uri: Uri,
    /// Version of the open document; `null` when the text was read from storage.
    pub version: Option<DocumentVersion>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSnapshot {
    pub uri: Uri,
    pub version: DocumentVersion,
    pub language_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEditParams {
    pub uri: Uri,
    /// Version the edits were computed against. Older versions are rebased; a conflict fails
    /// with `ContentModified`.
    pub base_version: DocumentVersion,
    /// Non-overlapping edits, positions relative to `baseVersion`.
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyEditResult {
    /// Version after the edit.
    pub version: DocumentVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedContent {
    pub text: String,
    pub language_id: Option<String>,
}

request!(
    ReadDocument,
    "document/read",
    PluginToHost,
    DocumentParams,
    DocumentText,
    "Current text: the open buffer if any, otherwise the stored file."
);
request!(
    DocumentSnapshotRequest,
    "document/snapshot",
    PluginToHost,
    DocumentParams,
    DocumentSnapshot,
    "Full state of an open document; used to resynchronize."
);
request!(
    ApplyEdit,
    "document/applyEdit",
    PluginToHost,
    ApplyEditParams,
    ApplyEditResult,
    "Apply edits as one transaction."
);
request!(
    OpenDocument,
    "document/open",
    PluginToHost,
    DocumentParams,
    DocumentSnapshot,
    "Open a document in the core without showing it."
);
request!(
    SaveDocument,
    "document/save",
    PluginToHost,
    DocumentParams,
    (),
    "Save an open document."
);
request!(
    ResolveContent,
    "document/resolveContent",
    HostToPlugin,
    DocumentParams,
    ResolvedContent,
    "Content of a read-only document in a URI scheme the plugin owns."
);
