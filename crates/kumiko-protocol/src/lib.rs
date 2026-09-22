//! Kumiko Plugin Protocol (KPP): messages, method catalog, versions and capabilities.
//!
//! One catalog serves both kinds of plugins: process plugins exchange JSON-RPC 2.0 messages in
//! binary frames (see `kumiko-ipc`), WASM plugins call the same methods through the
//! `kumiko:plugin` WIT world with the same JSON payloads (spec, section 16).
//!
//! Every method is a type implementing [`Request`] or [`Notification`]; [`catalog`] lists
//! them all and [`schema::protocol_schema`] turns the catalog into the published JSON Schema.
//! Any change here must be mirrored in `docs/protocol/v1.md`, `sdk/schema/kpp.schema.json`,
//! `sdk/wit/kumiko-plugin/` and `CHANGELOG-protocol.md`; tests enforce the first three.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod catalog;
pub mod common;
pub mod events;
pub mod jsonrpc;
pub mod methods;
pub mod schema;

pub use catalog::{MethodInfo, MethodKind, catalog};
pub use jsonrpc::{
    ErrorCode, Message, MessageError, NotificationMessage, RequestId, RequestMessage,
    ResponseError, ResponseMessage,
};

/// Protocol version. Stays 0.x until the MVP freeze, where it becomes 1.0.0 (spec, section 16).
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Who sends a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    /// The host (core) sends, the plugin handles.
    HostToPlugin,
    /// The plugin sends, the host handles.
    PluginToHost,
    /// Either side may send.
    Both,
}

/// A method that expects a response.
pub trait Request {
    const METHOD: &'static str;
    const DIRECTION: Direction;
    type Params: Serialize + DeserializeOwned + JsonSchema;
    type Result: Serialize + DeserializeOwned + JsonSchema;
}

/// A method without a response.
pub trait Notification {
    const METHOD: &'static str;
    const DIRECTION: Direction;
    type Params: Serialize + DeserializeOwned + JsonSchema;
}

/// Declares a request type: `request!(Name, "ns/method", Direction, Params, Result, "doc");`
macro_rules! request {
    ($name:ident, $method:literal, $dir:ident, $params:ty, $result:ty, $doc:literal) => {
        #[doc = concat!("`", $method, "`: ", $doc)]
        #[derive(Debug)]
        pub enum $name {}

        impl $crate::Request for $name {
            const METHOD: &'static str = $method;
            const DIRECTION: $crate::Direction = $crate::Direction::$dir;
            type Params = $params;
            type Result = $result;
        }

        impl $crate::catalog::Described for $name {
            const DOC: &'static str = $doc;
        }
    };
}

/// Declares a notification type: `notification!(Name, "ns/method", Direction, Params, "doc");`
macro_rules! notification {
    ($name:ident, $method:literal, $dir:ident, $params:ty, $doc:literal) => {
        #[doc = concat!("`", $method, "`: ", $doc)]
        #[derive(Debug)]
        pub enum $name {}

        impl $crate::Notification for $name {
            const METHOD: &'static str = $method;
            const DIRECTION: $crate::Direction = $crate::Direction::$dir;
            type Params = $params;
        }

        impl $crate::catalog::Described for $name {
            const DOC: &'static str = $doc;
        }
    };
}

pub(crate) use {notification, request};
