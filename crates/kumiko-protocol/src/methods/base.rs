//! `$/` methods: cancellation, progress, logging, liveness and resynchronization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::ProgressToken;
use crate::jsonrpc::RequestId;
use crate::{notification, request};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelParams {
    /// Id of the request to cancel; it completes with `RequestCancelled`.
    pub id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressParams {
    pub token: ProgressToken,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogParams {
    pub level: LogLevel,
    pub message: String,
    /// Module or component inside the plugin.
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResyncParams {
    /// Topics whose subscriptions were dropped after a queue overflow.
    pub topics: Vec<String>,
}

notification!(
    CancelRequest,
    "$/cancelRequest",
    Both,
    CancelParams,
    "Cancel a pending request."
);
notification!(
    Progress,
    "$/progress",
    Both,
    ProgressParams,
    "Progress or partial results of a request that carried a progress token."
);
notification!(
    Log,
    "$/log",
    PluginToHost,
    LogParams,
    "A log record for the IDE log."
);
request!(Ping, "$/ping", Both, (), (), "Liveness check.");
notification!(
    Resync,
    "$/resync",
    HostToPlugin,
    ResyncParams,
    "Subscriptions were dropped: re-read state (`document/snapshot`) and subscribe again."
);
