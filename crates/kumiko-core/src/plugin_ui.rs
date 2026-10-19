//! Declarative UI of plugins (spec, section 20): plugins send data, the host renders it.
//! The core keeps the trees of their views, file decorations and status items, and announces
//! changes with `ui.changed`. Questions to the user (`ui/showInput`, `ui/showQuickPick`,
//! `ui/showMessage`) go to the UI as [`UiRequest`]s; without a UI they are cancelled.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use kumiko_protocol::events::UiChanged;
use kumiko_protocol::methods::ui::{
    FileDecoration, FileDecorationsParams, QuickPickResult, SetStatusItemParams, ShowInputParams,
    ShowMessageParams, ShowMessageResult, ShowQuickPickParams, StatusItem, TreeNode,
    TreeSetModelParams, TreeUpdateParams,
};
use kumiko_protocol::{ErrorCode, ResponseError};
use kumiko_service_api::{Event, EventSink};
use kumiko_types::{PluginId, Uri};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::CoreApi;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A question to the user, answered by the UI.
pub enum UiRequest {
    Input {
        plugin: PluginId,
        params: ShowInputParams,
        answer: oneshot::Sender<Option<String>>,
    },
    QuickPick {
        plugin: PluginId,
        params: ShowQuickPickParams,
        answer: oneshot::Sender<Vec<String>>,
    },
    Message {
        plugin: PluginId,
        params: ShowMessageParams,
        answer: oneshot::Sender<Option<String>>,
    },
}

/// A view's tree and the plugin that fills it.
type Tree = (PluginId, Vec<TreeNode>);

pub(crate) struct PluginUi {
    trees: Mutex<HashMap<String, Tree>>,
    decorations: Mutex<HashMap<Uri, Vec<(PluginId, FileDecoration)>>>,
    status: Mutex<BTreeMap<(PluginId, String), StatusItem>>,
    requests: mpsc::UnboundedSender<UiRequest>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<UiRequest>>>,
}

impl Default for PluginUi {
    fn default() -> Self {
        let (requests, receiver) = mpsc::unbounded_channel();
        Self {
            trees: Mutex::default(),
            decorations: Mutex::default(),
            status: Mutex::default(),
            requests,
            receiver: Mutex::new(Some(receiver)),
        }
    }
}

fn invalid(error: impl std::fmt::Display) -> ResponseError {
    ResponseError::new(ErrorCode::InvalidParams, error.to_string())
}

fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ResponseError> {
    serde_json::from_value(value).map_err(invalid)
}

fn value(v: impl serde::Serialize) -> Result<Value, ResponseError> {
    serde_json::to_value(v).map_err(invalid)
}

/// Replaces the children of `id` somewhere in `nodes`; `true` when it was found.
fn replace_children(nodes: &mut [TreeNode], id: &str, children: &[TreeNode]) -> bool {
    for node in nodes {
        if node.id == id {
            node.children = Some(children.to_vec());
            return true;
        }
        if let Some(inner) = &mut node.children
            && replace_children(inner, id, children)
        {
            return true;
        }
    }
    false
}

impl CoreApi {
    /// The roots of a plugin view (empty until the plugin sets them).
    pub fn view_tree(&self, view: &str) -> Vec<TreeNode> {
        lock(&self.inner.plugin_ui.trees)
            .get(view)
            .map(|(_, roots)| roots.clone())
            .unwrap_or_default()
    }

    /// File decorations of every plugin, by file.
    pub fn file_decorations(&self) -> HashMap<Uri, Vec<FileDecoration>> {
        lock(&self.inner.plugin_ui.decorations)
            .iter()
            .map(|(uri, list)| (uri.clone(), list.iter().map(|(_, d)| d.clone()).collect()))
            .collect()
    }

    /// Status items of plugins, in a stable order.
    pub fn status_items(&self) -> Vec<StatusItem> {
        let mut items: Vec<StatusItem> = lock(&self.inner.plugin_ui.status)
            .values()
            .cloned()
            .collect();
        items.sort_by_key(|i| std::cmp::Reverse(i.priority));
        items
    }

    /// The questions plugins ask the user; the UI takes them once and answers each.
    pub fn take_ui_requests(&self) -> Option<mpsc::UnboundedReceiver<UiRequest>> {
        lock(&self.inner.plugin_ui.receiver).take()
    }

    fn ui_changed(&self, what: &str, view: Option<String>) {
        self.inner.events.publish(Event::core(&UiChanged {
            what: what.to_owned(),
            view,
        }));
    }

    /// A UI took the questions: otherwise they are cancelled at once (headless).
    fn ui_listens(&self) -> bool {
        lock(&self.inner.plugin_ui.receiver).is_none()
    }

    /// The `ui/*` methods of a plugin.
    pub(crate) async fn ui_call(
        &self,
        plugin: PluginId,
        method: &str,
        request: Value,
    ) -> Result<Value, ResponseError> {
        let ui = &self.inner.plugin_ui;
        match method {
            "ui/tree/setModel" => {
                let p: TreeSetModelParams = params(request)?;
                self.check_view(&plugin, &p.view_id)?;
                lock(&ui.trees).insert(p.view_id.clone(), (plugin, p.roots));
                self.ui_changed("tree", Some(p.view_id));
                Ok(Value::Null)
            }
            "ui/tree/update" => {
                let p: TreeUpdateParams = params(request)?;
                self.check_view(&plugin, &p.view_id)?;
                let children = p.children.unwrap_or_default();
                {
                    let mut trees = lock(&ui.trees);
                    let tree = trees
                        .entry(p.view_id.clone())
                        .or_insert_with(|| (plugin.clone(), Vec::new()));
                    match &p.node_id {
                        None => tree.1 = children,
                        Some(id) => {
                            if !replace_children(&mut tree.1, id, &children) {
                                return Err(invalid(format!("no node {id} in {}", p.view_id)));
                            }
                        }
                    }
                }
                self.ui_changed("tree", Some(p.view_id));
                Ok(Value::Null)
            }
            "ui/fileDecorations" => {
                let p: FileDecorationsParams = params(request)?;
                {
                    let mut all = lock(&ui.decorations);
                    for uri in &p.clear {
                        if let Some(list) = all.get_mut(uri) {
                            list.retain(|(owner, _)| *owner != plugin);
                        }
                    }
                    for decoration in p.set {
                        let list = all.entry(decoration.uri.clone()).or_default();
                        list.retain(|(owner, _)| *owner != plugin);
                        list.push((plugin.clone(), decoration));
                    }
                    all.retain(|_, list| !list.is_empty());
                }
                self.ui_changed("decorations", None);
                Ok(Value::Null)
            }
            "ui/setStatusItem" => {
                let p: SetStatusItemParams = params(request)?;
                {
                    let mut status = lock(&ui.status);
                    let key = (plugin, p.id);
                    match p.item {
                        Some(item) => status.insert(key, item),
                        None => status.remove(&key),
                    };
                }
                self.ui_changed("status", None);
                Ok(Value::Null)
            }
            "ui/showInput" => {
                let (answer, answered) = oneshot::channel();
                let asked = ui.requests.send(UiRequest::Input {
                    plugin,
                    params: params(request)?,
                    answer,
                });
                let text = if asked.is_ok() && self.ui_listens() {
                    answered.await.ok().flatten()
                } else {
                    None
                };
                Ok(text.map_or(Value::Null, Value::String))
            }
            "ui/showQuickPick" => {
                let (answer, answered) = oneshot::channel();
                let asked = ui.requests.send(UiRequest::QuickPick {
                    plugin,
                    params: params(request)?,
                    answer,
                });
                let selected = if asked.is_ok() && self.ui_listens() {
                    answered.await.unwrap_or_default()
                } else {
                    Vec::new()
                };
                value(QuickPickResult { selected })
            }
            "ui/showMessage" => {
                let (answer, answered) = oneshot::channel();
                let asked = ui.requests.send(UiRequest::Message {
                    plugin,
                    params: params(request)?,
                    answer,
                });
                let action = if asked.is_ok() && self.ui_listens() {
                    answered.await.ok().flatten()
                } else {
                    None
                };
                value(ShowMessageResult { action })
            }
            other => Err(ResponseError::new(
                ErrorCode::MethodNotFound,
                format!("the host does not implement `{other}` yet"),
            )),
        }
    }

    /// A plugin fills only the views its manifest declares.
    fn check_view(&self, plugin: &PluginId, view: &str) -> Result<(), ResponseError> {
        let declared = self.plugins().get(plugin).is_some_and(|entry| {
            entry
                .manifest()
                .contributes
                .views
                .iter()
                .any(|v| v.id == view)
        });
        if declared {
            Ok(())
        } else {
            Err(ResponseError::new(
                ErrorCode::PermissionDenied,
                format!("{plugin} does not declare the view {view}"),
            ))
        }
    }

    /// Forgets what a plugin showed (it stopped or went away).
    pub(crate) fn forget_plugin_ui(&self, plugin: &PluginId) {
        let ui = &self.inner.plugin_ui;
        lock(&ui.trees).retain(|_, (owner, _)| owner != plugin);
        lock(&ui.decorations).retain(|_, list| {
            list.retain(|(owner, _)| owner != plugin);
            !list.is_empty()
        });
        lock(&ui.status).retain(|(owner, _), _| owner != plugin);
        self.ui_changed("tree", None);
        self.ui_changed("decorations", None);
        self.ui_changed("status", None);
    }
}
