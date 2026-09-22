//! Event subscriptions. Core topics and payloads are in [`crate::events`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::SubscriptionId;
use crate::{notification, request};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    /// Topics such as `document.changed`, or a namespace prefix such as `document.*`.
    pub topics: Vec<String>,
    /// Glob over URIs for URI-carrying topics such as `fs.changed`.
    pub glob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    pub subscription: SubscriptionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeParams {
    pub subscription: SubscriptionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PublishParams {
    /// Topic in the plugin's own namespace.
    pub topic: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventNotifyParams {
    pub subscription: SubscriptionId,
    pub topic: String,
    pub payload: Value,
}

request!(
    Subscribe,
    "event/subscribe",
    PluginToHost,
    SubscribeParams,
    SubscribeResult,
    "Subscribe to event topics."
);
request!(
    Unsubscribe,
    "event/unsubscribe",
    PluginToHost,
    UnsubscribeParams,
    (),
    "Cancel a subscription."
);
notification!(
    Publish,
    "event/publish",
    PluginToHost,
    PublishParams,
    "Publish an event in the plugin's own namespace."
);
notification!(
    EventNotify,
    "event/notify",
    HostToPlugin,
    EventNotifyParams,
    "Deliver an event to a subscriber."
);
