//! AccessKit attached from outside GPUI (ADR-003, condition 4): the workbench describes itself
//! as a plain tree ([`AccessNode`]), the bridge turns it into AccessKit updates.
//!
//! - Windows: `accesskit_windows::SubclassingAdapter` subclasses the window for UI Automation.
//!   It must exist before the window is first shown, so the window opens hidden.
//! - Linux: `accesskit_unix::Adapter` talks to AT-SPI over D-Bus and needs no window handle.

use std::sync::{Arc, Mutex};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Role, TreeId, TreeInfo,
    TreeUpdate,
};
use raw_window_handle::RawWindowHandle;

/// One node of the accessible tree.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessNode {
    pub id: u64,
    pub role: Role,
    pub label: String,
    pub children: Vec<u64>,
}

/// The whole tree: the first node is the window.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessTree {
    pub nodes: Vec<AccessNode>,
    pub focus: u64,
}

impl AccessTree {
    fn update(&self) -> TreeUpdate {
        let root = self.nodes.first().map_or(0, |n| n.id);
        let nodes = self
            .nodes
            .iter()
            .map(|n| {
                let mut node = Node::new(n.role);
                node.set_label(n.label.clone());
                node.set_children(n.children.iter().copied().map(NodeId).collect::<Vec<_>>());
                (NodeId(n.id), node)
            })
            .collect();
        let mut info = TreeInfo::new(NodeId(root));
        info.toolkit_name = Some("GPUI".to_owned());
        TreeUpdate {
            nodes,
            tree: Some(info),
            tree_id: TreeId::ROOT,
            focus: NodeId(self.focus),
        }
    }
}

struct Activation(Arc<Mutex<AccessTree>>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(self.0.lock().ok()?.update())
    }
}

struct IgnoreActions;

impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _request: ActionRequest) {}
}

#[cfg(target_os = "linux")]
struct IgnoreDeactivation;

#[cfg(target_os = "linux")]
impl accesskit::DeactivationHandler for IgnoreDeactivation {
    fn deactivate_accessibility(&mut self) {}
}

/// An adapter attached to the workbench window.
pub struct Bridge {
    tree: Arc<Mutex<AccessTree>>,
    #[cfg(windows)]
    adapter: accesskit_windows::SubclassingAdapter,
    #[cfg(target_os = "linux")]
    adapter: accesskit_unix::Adapter,
}

impl Bridge {
    /// Attaches to the window: Windows needs its handle, before the window is shown; Linux
    /// needs none. `None` when the platform or handle is not supported.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn attach(handle: Option<RawWindowHandle>, tree: AccessTree) -> Option<Self> {
        let tree = Arc::new(Mutex::new(tree));
        let activation = Activation(tree.clone());
        #[cfg(windows)]
        let adapter = {
            let Some(RawWindowHandle::Win32(win32)) = handle else {
                return None;
            };
            let hwnd = accesskit_windows::HWND(win32.hwnd.get() as *mut core::ffi::c_void);
            accesskit_windows::SubclassingAdapter::new(hwnd, activation, IgnoreActions)
        };
        #[cfg(target_os = "linux")]
        let adapter = accesskit_unix::Adapter::new(activation, IgnoreActions, IgnoreDeactivation);
        #[cfg(not(any(windows, target_os = "linux")))]
        return None;
        #[cfg(any(windows, target_os = "linux"))]
        Some(Self { tree, adapter })
    }

    /// Publishes a changed tree to assistive technologies, if any listen.
    pub fn update(&mut self, tree: AccessTree) {
        let Ok(mut current) = self.tree.lock() else {
            return;
        };
        if *current == tree {
            return;
        }
        *current = tree;
        let update = || current.update();
        #[cfg(windows)]
        if let Some(events) = self.adapter.update_if_active(update) {
            events.raise();
        }
        #[cfg(target_os = "linux")]
        self.adapter.update_if_active(update);
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = update;
    }

    /// The window gained or lost focus (AT-SPI needs to know; Windows asks the window).
    pub fn window_focused(&mut self, focused: bool) {
        #[cfg(target_os = "linux")]
        self.adapter.update_window_focus_state(focused);
        #[cfg(not(target_os = "linux"))]
        let _ = focused;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_becomes_an_update_rooted_at_the_window() {
        let tree = AccessTree {
            nodes: vec![
                AccessNode {
                    id: 1,
                    role: Role::Window,
                    label: "Kumiko".into(),
                    children: vec![2],
                },
                AccessNode {
                    id: 2,
                    role: Role::Status,
                    label: "Status".into(),
                    children: vec![],
                },
            ],
            focus: 2,
        };
        let update = tree.update();
        assert_eq!(update.nodes.len(), 2);
        assert_eq!(update.focus, NodeId(2));
        assert_eq!(update.tree.unwrap().root, NodeId(1));
    }
}
