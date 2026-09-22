//! Handshake, shutdown and optional state hand-over.
//!
//! `initialize` → `initialized` → work in both directions → `shutdown` → `exit`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::PluginId;

use crate::common::WorkspaceFolder;
use crate::{notification, request};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Protocol version of the host, e.g. `0.1.0`.
    pub api_version: String,
    /// Id of the plugin being initialized, as in its manifest.
    pub plugin_id: PluginId,
    pub host_capabilities: HostCapabilities,
    pub workspace: WorkspaceInfo,
    /// Current values of the plugin's own settings namespace.
    pub config: Value,
    /// UI locale, e.g. `en` or `ru`.
    pub locale: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostCapabilities {
    /// Plugin-to-host methods the host implements. Minor versions add methods; plugins check
    /// this list instead of comparing versions.
    pub methods: Vec<String>,
    pub experimental: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginCapabilities {
    /// Optional host-to-plugin methods the plugin handles, e.g. `state/save`,
    /// `document/resolveContent`, `task/provide`, `ui/tree/getChildren`.
    pub methods: Vec<String>,
    /// LSP methods the plugin intercepts through `lsp/middleware/request`.
    #[serde(default)]
    pub lsp_middleware: Vec<String>,
    pub experimental: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceInfo {
    pub folders: Vec<WorkspaceFolder>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Protocol version of the plugin; a different major version fails the handshake.
    pub api_version: String,
    pub plugin_capabilities: PluginCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SavedState {
    /// Opaque state the plugin wants back after a reload.
    pub state: Value,
}

request!(
    Initialize,
    "initialize",
    HostToPlugin,
    InitializeParams,
    InitializeResult,
    "Handshake: versions, capabilities, workspace, settings and locale."
);
notification!(
    Initialized,
    "initialized",
    HostToPlugin,
    (),
    "Handshake finished; the plugin may start sending requests."
);
request!(
    Shutdown,
    "shutdown",
    HostToPlugin,
    (),
    (),
    "Stop work and release resources; the host sends `exit` afterwards."
);
notification!(Exit, "exit", HostToPlugin, (), "Terminate now.");
request!(
    StateSave,
    "state/save",
    HostToPlugin,
    (),
    SavedState,
    "Optional: return state to hand over to the next instance."
);
request!(
    StateRestore,
    "state/restore",
    HostToPlugin,
    SavedState,
    (),
    "Optional: restore state saved by a previous instance."
);
