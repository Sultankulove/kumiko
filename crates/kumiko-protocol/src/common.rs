//! Types shared by several namespaces.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::{CommandId, Uri};

/// A workspace folder the IDE has open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceFolder {
    pub uri: Uri,
    pub name: String,
}

/// Subscription handle returned by `event/subscribe` and `fs/watch`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SubscriptionId(pub u64);

/// Binary stream carried by frames of kind 1 (data) and 2 (close).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct StreamId(pub u32);

/// Token that ties progress reports to an operation, chosen by the side that starts it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ProgressToken(pub String);

/// A command with arguments, e.g. the action of a status item or a tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommandCall {
    pub command: CommandId,
    #[serde(default)]
    pub args: Vec<Value>,
}

/// Bytes in JSON: UTF-8 text as is, anything else base64-encoded (RFC 4648, with padding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Content {
    Text(String),
    Base64(String),
}
