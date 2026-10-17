//! Core events: stable topics, versioned payloads and delivery classes (spec, section 11).
//!
//! Plugins receive events through `event/notify`. Plugin-defined topics live in the plugin's
//! own namespace and carry free-form payloads; the core topics below have typed payloads.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use kumiko_types::{DocumentVersion, InterfaceId, PluginId, TextEdit, Uri};

use crate::common::WorkspaceFolder;
use crate::methods::task::TaskExecutionId;

/// Delivery guarantee of an event topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryClass {
    /// Class A: lossless, ordered per source. On subscriber overflow the host drops the
    /// subscription and sends `$/resync`; the subscriber re-reads state and subscribes again.
    State,
    /// Class B: at-most-once, coalesced.
    Notification,
}

/// A core event with a typed payload.
pub trait CoreEvent: Serialize + DeserializeOwned + JsonSchema {
    const TOPIC: &'static str;
    const CLASS: DeliveryClass;
    const DOC: &'static str;
}

macro_rules! core_event {
    ($name:ident, $topic:literal, $class:ident, $doc:literal) => {
        impl CoreEvent for $name {
            const TOPIC: &'static str = $topic;
            const CLASS: DeliveryClass = DeliveryClass::$class;
            const DOC: &'static str = $doc;
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceFolderAdded {
    pub folder: WorkspaceFolder,
}
core_event!(
    WorkspaceFolderAdded,
    "workspace.folderAdded",
    State,
    "A workspace folder was opened."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceFolderRemoved {
    pub folder: WorkspaceFolder,
}
core_event!(
    WorkspaceFolderRemoved,
    "workspace.folderRemoved",
    State,
    "A workspace folder was closed."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOpened {
    pub uri: Uri,
    pub version: DocumentVersion,
    pub language_id: Option<String>,
}
core_event!(
    DocumentOpened,
    "document.opened",
    State,
    "A document was opened in the core."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentChanged {
    pub uri: Uri,
    /// Version after the change; exactly one more than the previous event for this document.
    pub version: DocumentVersion,
    /// Edits in application order, positions relative to the previous version.
    pub changes: Vec<TextEdit>,
}
core_event!(
    DocumentChanged,
    "document.changed",
    State,
    "A transaction was applied to a document."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentSaved {
    pub uri: Uri,
    pub version: DocumentVersion,
}
core_event!(
    DocumentSaved,
    "document.saved",
    State,
    "A document was written to storage."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentClosed {
    pub uri: Uri,
}
core_event!(
    DocumentClosed,
    "document.closed",
    State,
    "A document was closed in the core."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FsChangeKind {
    Created,
    Changed,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsChange {
    pub uri: Uri,
    pub kind: FsChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsChanged {
    pub changes: Vec<FsChange>,
}
core_event!(
    FsChanged,
    "fs.changed",
    Notification,
    "Batch of file system changes, coalesced over 50-100 ms."
);

/// Plugin lifecycle state (spec, section 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginState {
    Installed,
    Disabled,
    Enabled,
    Starting,
    Running,
    Stopping,
    Crashed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PluginStateChanged {
    pub plugin: PluginId,
    pub state: PluginState,
    /// Why the plugin is in this state, e.g. a missing required interface.
    pub reason: Option<String>,
}
core_event!(
    PluginStateChanged,
    "plugin.stateChanged",
    State,
    "A plugin moved to another lifecycle state."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStarted {
    pub execution: TaskExecutionId,
    pub label: String,
}
core_event!(
    TaskStarted,
    "task.started",
    State,
    "A task execution started."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskFinished {
    pub execution: TaskExecutionId,
    /// `null` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}
core_event!(
    TaskFinished,
    "task.finished",
    State,
    "A task execution finished."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigChanged {
    pub keys: Vec<String>,
}
core_event!(
    ConfigChanged,
    "config.changed",
    State,
    "Effective values of these settings changed."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InterfaceAvailabilityChanged {
    pub interface: InterfaceId,
    pub available: bool,
}
core_event!(
    InterfaceAvailabilityChanged,
    "interface.availabilityChanged",
    State,
    "A provider of an inter-plugin interface appeared or disappeared."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsChanged {
    /// Files whose merged diagnostics changed; read them from the Language service.
    pub uris: Vec<Uri>,
}
core_event!(
    DiagnosticsChanged,
    "language.diagnosticsChanged",
    State,
    "Diagnostics of these files changed."
);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LanguageStatusChanged {}
core_event!(
    LanguageStatusChanged,
    "language.statusChanged",
    Notification,
    "Language servers started, stopped or reported progress; read the status again."
);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TerminalId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalOpened {
    pub terminal: TerminalId,
    pub title: String,
}
core_event!(
    TerminalOpened,
    "terminal.opened",
    State,
    "A terminal started; the UI shows it."
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalClosed {
    pub terminal: TerminalId,
    pub exit_code: Option<i32>,
}
core_event!(
    TerminalClosed,
    "terminal.closed",
    State,
    "A terminal's shell ended."
);
