//! Declarative UI models (spec, section 20): the plugin sends data, only the host renders.
//! Icons and colors are theme tokens, e.g. `icon: "git-branch"`, `color: "git.modified"`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use kumiko_types::{Range, Uri};

use crate::common::{CommandCall, ProgressToken};
use crate::{notification, request};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShowMessageParams {
    pub level: MessageLevel,
    pub message: String,
    /// Button labels; the chosen one is returned.
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShowMessageResult {
    /// `null` when the message was dismissed.
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuickPickItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub detail: Option<String>,
    /// Initially selected (multi-select only).
    #[serde(default)]
    pub picked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShowQuickPickParams {
    pub items: Vec<QuickPickItem>,
    pub placeholder: Option<String>,
    #[serde(default)]
    pub can_pick_many: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuickPickResult {
    /// Ids of the chosen items.
    pub selected: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShowInputParams {
    pub prompt: Option<String>,
    pub placeholder: Option<String>,
    /// Initial value.
    pub value: Option<String>,
    /// Hide the typed text.
    #[serde(default)]
    pub password: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StatusAlignment {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusItem {
    pub text: String,
    pub tooltip: Option<String>,
    pub command: Option<CommandCall>,
    pub alignment: StatusAlignment,
    /// Higher values are placed closer to the edge.
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetStatusItemParams {
    /// Item id, unique within the plugin.
    pub id: String,
    /// `null` removes the item.
    pub item: Option<StatusItem>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Collapsible {
    /// A leaf.
    #[default]
    None,
    Collapsed,
    Expanded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// Unique within the view.
    pub id: String,
    pub label: String,
    /// Theme icon token.
    pub icon: Option<String>,
    pub description: Option<String>,
    pub tooltip: Option<String>,
    #[serde(default)]
    pub collapsible: Collapsible,
    /// Matched by `viewItem == ...` in when-expressions of context menus.
    pub context_value: Option<String>,
    /// Run when the node is activated.
    pub command: Option<CommandCall>,
    /// Children known up front; `null` for a collapsible node means the host asks
    /// `ui/tree/getChildren` when the node is expanded.
    pub children: Option<Vec<TreeNode>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeSetModelParams {
    /// View declared in the plugin manifest.
    pub view_id: String,
    pub roots: Vec<TreeNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeUpdateParams {
    pub view_id: String,
    /// Node whose children changed; `null` for the roots.
    pub node_id: Option<String>,
    /// New children; `null` makes the host ask `ui/tree/getChildren` again.
    pub children: Option<Vec<TreeNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeGetChildrenParams {
    pub view_id: String,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeChildren {
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileDecoration {
    pub uri: Uri,
    /// One or two characters, e.g. `M`.
    pub badge: Option<String>,
    /// Theme color token.
    pub color: Option<String>,
    pub tooltip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileDecorationsParams {
    /// Decorations to add or replace.
    #[serde(default)]
    pub set: Vec<FileDecoration>,
    /// URIs whose decorations from this plugin are removed.
    #[serde(default)]
    pub clear: Vec<Uri>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressBeginParams {
    pub token: ProgressToken,
    pub title: String,
    pub message: Option<String>,
    /// 0..=100; `null` for indeterminate progress.
    pub percentage: Option<u8>,
    /// Show a cancel button; cancellation arrives as `ui/progress/cancel`.
    #[serde(default)]
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressReportParams {
    pub token: ProgressToken,
    pub message: Option<String>,
    pub percentage: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressEndParams {
    pub token: ProgressToken,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressCancelParams {
    pub token: ProgressToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpenDocumentInEditorParams {
    pub uri: Uri,
    /// Range to select and reveal.
    pub selection: Option<Range>,
    /// Open in a preview tab that the next preview replaces.
    #[serde(default)]
    pub preview: bool,
}

request!(
    ShowMessage,
    "ui/showMessage",
    PluginToHost,
    ShowMessageParams,
    ShowMessageResult,
    "Show a notification, optionally with action buttons."
);
request!(
    ShowQuickPick,
    "ui/showQuickPick",
    PluginToHost,
    ShowQuickPickParams,
    Option<QuickPickResult>,
    "Let the user pick from a list; `null` when dismissed."
);
request!(
    ShowInput,
    "ui/showInput",
    PluginToHost,
    ShowInputParams,
    Option<String>,
    "Ask the user for a line of text; `null` when dismissed."
);
request!(
    SetStatusItem,
    "ui/setStatusItem",
    PluginToHost,
    SetStatusItemParams,
    (),
    "Create, update or remove a status bar item."
);
request!(
    TreeSetModel,
    "ui/tree/setModel",
    PluginToHost,
    TreeSetModelParams,
    (),
    "Replace the whole model of a tree view."
);
request!(
    TreeUpdate,
    "ui/tree/update",
    PluginToHost,
    TreeUpdateParams,
    (),
    "Replace or invalidate the children of one node."
);
request!(
    TreeGetChildren,
    "ui/tree/getChildren",
    HostToPlugin,
    TreeGetChildrenParams,
    TreeChildren,
    "Lazily load the children of an expanded node."
);
notification!(
    FileDecorations,
    "ui/fileDecorations",
    PluginToHost,
    FileDecorationsParams,
    "Set or clear badges and colors of files in trees and tabs."
);
request!(
    ProgressBegin,
    "ui/progress/begin",
    PluginToHost,
    ProgressBeginParams,
    (),
    "Start showing progress of a long operation."
);
notification!(
    ProgressReport,
    "ui/progress/report",
    PluginToHost,
    ProgressReportParams,
    "Update progress."
);
notification!(
    ProgressEnd,
    "ui/progress/end",
    PluginToHost,
    ProgressEndParams,
    "Finish progress."
);
notification!(
    ProgressCancel,
    "ui/progress/cancel",
    HostToPlugin,
    ProgressCancelParams,
    "The user cancelled a cancellable progress."
);
request!(
    OpenDocumentInEditor,
    "ui/openDocument",
    PluginToHost,
    OpenDocumentInEditorParams,
    (),
    "Show a document in the editor."
);
