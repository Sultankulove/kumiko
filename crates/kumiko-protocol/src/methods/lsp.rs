//! Language servers: the core owns the generic LSP client, a language plugin describes how to
//! start its server and intercepts server-specific methods (spec, section 18).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::Uri;

use crate::request;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveLaunchParams {
    /// Server id from `contributes.languageServers`.
    pub server_id: String,
    pub language_id: String,
    pub workspace_folder: Uri,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub initialization_options: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspMessageParams {
    pub server_id: String,
    /// LSP method name.
    pub method: String,
    /// LSP params, positions in the server's encoding.
    pub params: Value,
}

/// What the core does with an intercepted message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum MiddlewareResult {
    /// Answer with this result instead of the default handling.
    Respond { result: Value },
    /// Continue with the default handling, with possibly modified params.
    Forward { params: Value },
}

request!(
    ResolveLaunch,
    "lsp/resolveLaunch",
    HostToPlugin,
    ResolveLaunchParams,
    LaunchSpec,
    "Command line and initialization options for a language server."
);
request!(
    LspMiddleware,
    "lsp/middleware/request",
    HostToPlugin,
    LspMessageParams,
    MiddlewareResult,
    "Intercept a message the plugin declared in `lspMiddleware`."
);
request!(
    LspSendRequest,
    "lsp/sendRequest",
    PluginToHost,
    LspMessageParams,
    Value,
    "Send a request to a running language server."
);
