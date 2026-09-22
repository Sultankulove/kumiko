//! Commands and context keys.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::CommandId;

use crate::common::CommandCall;
use crate::request;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterCommandParams {
    /// A command created at runtime; commands from the manifest need no registration.
    pub command: CommandId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetContextParams {
    /// Context key in the plugin's own namespace, e.g. `java.serverReady`.
    pub key: String,
    /// `null` removes the key.
    pub value: Value,
}

request!(
    RegisterCommand,
    "command/register",
    PluginToHost,
    RegisterCommandParams,
    (),
    "Register a runtime command handled by this plugin; released when the plugin stops."
);
request!(
    ExecuteCommand,
    "command/execute",
    PluginToHost,
    CommandCall,
    Value,
    "Execute any command (core or another plugin) and return its result."
);
request!(
    SetContext,
    "command/setContext",
    PluginToHost,
    SetContextParams,
    (),
    "Set a context key used by when-expressions."
);
request!(
    InvokeCommand,
    "command/invoke",
    HostToPlugin,
    CommandCall,
    Value,
    "Run a command contributed or registered by this plugin."
);
