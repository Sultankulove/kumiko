//! Versioned inter-plugin interfaces routed by the core (spec, section 14).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::{InterfaceId, PluginId};

use crate::request;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceCallParams {
    pub interface: InterfaceId,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceInvokeParams {
    pub interface: InterfaceId,
    pub method: String,
    pub params: Value,
    /// The consumer that made the call.
    pub caller: PluginId,
}

request!(
    ServiceCall,
    "service/call",
    PluginToHost,
    ServiceCallParams,
    Value,
    "Call a method of an interface declared in `[[consumes]]`."
);
request!(
    ServiceInvoke,
    "service/invoke",
    HostToPlugin,
    ServiceInvokeParams,
    Value,
    "Handle a call to an interface declared in `[[provides]]`."
);
