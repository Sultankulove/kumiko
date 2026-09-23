//! Layered settings (spec, section 22).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kumiko_types::Uri;

use crate::events::ConfigChanged;
use crate::{notification, request};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigScope {
    /// Resource whose workspace folder settings apply.
    pub uri: Option<Uri>,
    /// Language whose `[language.<id>]` overrides apply.
    pub language_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetParams {
    /// A key (`editor.fontSize`) or a section prefix (`java`) returning an object.
    pub key: String,
    pub scope: Option<ConfigScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfigTarget {
    User,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigUpdateParams {
    /// Key in the plugin's own namespace.
    pub key: String,
    /// `null` removes the value from the target layer.
    pub value: Value,
    pub target: ConfigTarget,
}

request!(
    ConfigGet,
    "config/get",
    PluginToHost,
    ConfigGetParams,
    Value,
    "Effective value of a setting or section."
);
request!(
    ConfigUpdate,
    "config/update",
    PluginToHost,
    ConfigUpdateParams,
    (),
    "Change a setting in the plugin's namespace."
);
notification!(
    ConfigChangedNotification,
    "config/changed",
    HostToPlugin,
    ConfigChanged,
    "Settings of the plugin's namespace changed."
);
