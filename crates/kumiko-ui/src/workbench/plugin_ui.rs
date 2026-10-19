//! What plugins show (spec, section 20): the Commit tool window with the trees of their views,
//! their status items, and the questions they ask (`ui/showInput`, `ui/showQuickPick`,
//! `ui/showMessage`) in the Quick Open popup.

use gpui::{AnyElement, ClickEvent, Context, div, prelude::*, px, rgba};
use kumiko_core::plugin_ui::UiRequest;
use kumiko_core::protocol::methods::ui::{Collapsible, TreeNode};
use serde_json::Value;
use tokio::sync::oneshot;

use super::{FocusTarget, Workbench};
use crate::quick_open::{Action, Item, Mode, QuickOpen};

/// A question waiting for the user's answer.
pub(super) enum Ask {
    Input(oneshot::Sender<Option<String>>),
    Pick {
        answer: oneshot::Sender<Vec<String>>,
        ids: Vec<String>,
    },
}

impl Ask {
    /// The user left the popup without answering.
    pub(super) fn cancel(self) {
        match self {
            Ask::Input(answer) => {
                let _ = answer.send(None);
            }
            Ask::Pick { answer, .. } => {
                let _ = answer.send(Vec::new());
            }
        }
    }
}

/// A row of the flattened view: the node and how deep it is.
struct Line {
    node: TreeNode,
    depth: usize,
}

impl Workbench {
    /// Takes the plugins' questions once; each opens the popup.
    pub(super) fn listen_to_plugins(&self, cx: &mut Context<Self>) {
        let Some(mut requests) = self.core.take_ui_requests() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            while let Some(request) = requests.recv().await {
                if this.update(cx, |w, cx| w.plugin_asks(request, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn plugin_asks(&mut self, request: UiRequest, cx: &mut Context<Self>) {
        // One question at a time; an earlier unanswered one is cancelled.
        if let Some(old) = self.ask.take() {
            old.cancel();
        }
        match request {
            UiRequest::Input { params, answer, .. } => {
                let text = params.value.clone().unwrap_or_default();
                self.ask_prompt = params.prompt.or(params.placeholder).unwrap_or_default();
                self.ask = Some(Ask::Input(answer));
                self.quick_open = Some(QuickOpen::prompt(Mode::Ask, &text));
            }
            UiRequest::QuickPick { params, answer, .. } => {
                self.ask_prompt = params.placeholder.unwrap_or_default();
                self.ask_items = params
                    .items
                    .iter()
                    .map(|i| Item {
                        label: i.label.clone(),
                        highlights: Vec::new(),
                        detail: i.description.clone().unwrap_or_default(),
                        keys: None,
                        action: Action::None,
                    })
                    .enumerate()
                    .map(|(n, mut item)| {
                        item.action = Action::Answer(Some(n));
                        item
                    })
                    .collect();
                let ids = params.items.into_iter().map(|i| i.id).collect();
                self.ask = Some(Ask::Pick { answer, ids });
                self.quick_open = Some(QuickOpen::picker(Mode::Pick));
            }
            UiRequest::Message { params, answer, .. } => {
                // A message has no popup: the status bar shows it; buttons are not offered.
                self.message = Some(params.message);
                let _ = answer.send(None);
                cx.notify();
                return;
            }
        }
        self.open_menu = None;
        self.refresh_quick();
        self.focus_request = Some(FocusTarget::Quick);
        cx.notify();
    }

    pub(super) fn ask_items(&self, query: &str) -> Vec<Item> {
        match self.ask {
            Some(Ask::Input(_)) => vec![Item {
                label: if self.ask_prompt.is_empty() {
                    self.t("ask.prompt")
                } else {
                    self.ask_prompt.clone()
                },
                highlights: Vec::new(),
                detail: String::new(),
                keys: Some("Enter".into()),
                action: if query.is_empty() {
                    Action::None
                } else {
                    Action::Answer(None)
                },
            }],
            _ => Vec::new(),
        }
    }

    pub(super) fn pick_items(&self, query: &str) -> Vec<Item> {
        let query = query.to_lowercase();
        self.ask_items
            .iter()
            .filter(|i| i.label.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    /// The user chose: the answer goes to the plugin.
    pub(super) fn answer(&mut self, choice: Option<usize>, typed: String) {
        match self.ask.take() {
            Some(Ask::Input(answer)) => {
                let _ = answer.send(Some(typed));
            }
            Some(Ask::Pick { answer, ids }) => {
                let selected = choice
                    .and_then(|n| ids.get(n).cloned())
                    .into_iter()
                    .collect();
                let _ = answer.send(selected);
            }
            None => {}
        }
    }

    // ---- the Commit tool window -----------------------------------------------------------

    fn commit_lines(&self) -> Vec<Line> {
        fn walk(
            nodes: &[TreeNode],
            depth: usize,
            collapsed: &std::collections::HashSet<String>,
            out: &mut Vec<Line>,
        ) {
            for node in nodes {
                out.push(Line {
                    node: node.clone(),
                    depth,
                });
                let open = match node.collapsible {
                    Collapsible::Expanded => !collapsed.contains(&node.id),
                    Collapsible::Collapsed => collapsed.contains(&format!("open:{}", node.id)),
                    Collapsible::None => false,
                };
                if open && let Some(children) = &node.children {
                    walk(children, depth + 1, collapsed, out);
                }
            }
        }
        let mut lines = Vec::new();
        for view in &self.plugin_views {
            walk(
                &self.core.view_tree(view),
                0,
                &self.tree_toggles,
                &mut lines,
            );
        }
        lines
    }

    pub(super) fn commit_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let lines = self.commit_lines();
        if lines.is_empty() {
            return div()
                .p_2()
                .text_color(rgba(p.muted))
                .child(self.t("commit.none"))
                .into_any_element();
        }
        let rows = lines.into_iter().enumerate().map(|(i, line)| {
            let node = line.node;
            let color = node
                .icon
                .as_deref()
                .and_then(|token| p.token(token))
                .unwrap_or(p.foreground);
            let arrow = match node.collapsible {
                Collapsible::None => " ",
                _ => {
                    let open = match node.collapsible {
                        Collapsible::Expanded => !self.tree_toggles.contains(&node.id),
                        _ => self.tree_toggles.contains(&format!("open:{}", node.id)),
                    };
                    if open { "▾" } else { "▸" }
                }
            };
            let toggle_id = node.id.clone();
            let expanded_default = node.collapsible == Collapsible::Expanded;
            let command = node.command.clone();
            div()
                .id(("commit-row", i))
                .h(px(super::ROW_HEIGHT))
                .pl(px(6.0 + line.depth as f32 * 16.0))
                .flex()
                .items_center()
                .gap_1()
                .whitespace_nowrap()
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if node.collapsible != Collapsible::None {
                        let key = if expanded_default {
                            toggle_id.clone()
                        } else {
                            format!("open:{toggle_id}")
                        };
                        if !this.tree_toggles.remove(&key) {
                            this.tree_toggles.insert(key);
                        }
                    }
                    if let Some(call) = &command {
                        let args: Vec<Value> = call.args.clone();
                        this.execute_id(call.command.as_str(), args, cx);
                    }
                    cx.notify();
                }))
                .child(div().w(px(10.0)).text_color(rgba(p.muted)).child(arrow))
                .child(div().text_color(rgba(color)).child(node.label.clone()))
                .children(
                    node.description
                        .clone()
                        .filter(|d| !d.is_empty())
                        .map(|d| div().text_color(rgba(p.muted)).child(d)),
                )
        });
        div()
            .id("commit-view")
            .flex_1()
            .overflow_y_scroll()
            .py_1()
            .children(rows)
            .into_any_element()
    }

    /// The views plugins declare, in manifest order.
    pub(super) fn refresh_plugin_views(&mut self) {
        self.plugin_views = self
            .core
            .plugins()
            .entries()
            .filter(|e| e.is_ready())
            .flat_map(|e| e.manifest().contributes.views.iter().map(|v| v.id.clone()))
            .collect();
    }

    /// Shows the view; the plugin refreshes it first (its refresh command).
    pub(super) fn refresh_commit(&mut self, cx: &mut Context<Self>) {
        let commands: Vec<String> = self
            .core
            .plugins()
            .entries()
            .filter(|e| e.is_ready())
            .flat_map(|e| {
                e.manifest()
                    .contributes
                    .views
                    .iter()
                    .filter_map(|v| v.refresh_command.as_ref().map(|c| c.to_string()))
            })
            .collect();
        for command in commands {
            self.execute_id(&command, Vec::new(), cx);
        }
    }

    /// Status items of plugins at the right of the status bar.
    pub(super) fn plugin_status(&self) -> Vec<String> {
        self.core
            .status_items()
            .into_iter()
            .map(|i| i.text)
            .collect()
    }
}
