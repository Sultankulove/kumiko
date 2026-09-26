//! AccessKit attached from outside a toolkit (spec, section 20: accessibility "without forking
//! the toolkit"). The file tree becomes an accessible tree: a window with a tree of tree items
//! and an editor; keyboard focus follows the selected item.
//!
//! - Windows: `accesskit_windows::SubclassingAdapter` subclasses the toolkit's window (UI
//!   Automation). It must be created before the window is first shown, so the prototype opens
//!   its window hidden, attaches the bridge and shows it.
//! - Linux: `accesskit_unix::Adapter` talks to AT-SPI over D-Bus and needs no window handle.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Role, TreeId, TreeInfo,
    TreeUpdate,
};
use raw_window_handle::RawWindowHandle;
use workload::Node as FileNode;

const WINDOW: NodeId = NodeId(0);
const TREE: NodeId = NodeId(1);
const EDITOR: NodeId = NodeId(2);

fn item(index: usize) -> NodeId {
    NodeId(10 + index as u64)
}

/// The whole accessible tree with the item `selected` focused.
pub fn tree_update(toolkit: &str, files: &[FileNode], selected: usize) -> TreeUpdate {
    let mut nodes = Vec::with_capacity(files.len() + 3);
    let mut window = Node::new(Role::Window);
    window.set_label(format!("Kumiko UI spike: {toolkit}"));
    window.set_children(vec![TREE, EDITOR]);
    nodes.push((WINDOW, window));

    let mut tree = Node::new(Role::Tree);
    tree.set_label("Project");
    tree.set_children((0..files.len()).map(item).collect::<Vec<_>>());
    nodes.push((TREE, tree));

    let mut editor = Node::new(Role::MultilineTextInput);
    editor.set_label("Editor");
    nodes.push((EDITOR, editor));

    for (index, file) in files.iter().enumerate() {
        let mut node = Node::new(Role::TreeItem);
        node.set_label(file.name.clone());
        node.set_level(file.depth + 1);
        node.set_selected(index == selected);
        node.add_action(Action::Focus);
        nodes.push((item(index), node));
    }
    let mut info = TreeInfo::new(WINDOW);
    info.toolkit_name = Some(toolkit.to_owned());
    TreeUpdate {
        nodes,
        tree: Some(info),
        tree_id: TreeId::ROOT,
        focus: item(selected),
    }
}

struct Activation {
    toolkit: String,
    files: Arc<Vec<FileNode>>,
    selected: Arc<AtomicUsize>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let selected = self.selected.load(Ordering::Relaxed);
        Some(tree_update(&self.toolkit, &self.files, selected))
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

/// An adapter attached to a toolkit window.
pub struct Bridge {
    toolkit: String,
    files: Arc<Vec<FileNode>>,
    selected: Arc<AtomicUsize>,
    #[cfg(windows)]
    adapter: accesskit_windows::SubclassingAdapter,
    #[cfg(target_os = "linux")]
    adapter: accesskit_unix::Adapter,
}

impl Bridge {
    /// Attaches to the window; on Windows it needs the handle and the window must not have been
    /// shown yet (Linux needs no handle). `None` when the
    /// platform or the handle is not supported.
    #[allow(unused_variables)]
    pub fn attach(
        toolkit: &str,
        handle: Option<RawWindowHandle>,
        files: Vec<FileNode>,
    ) -> Option<Self> {
        let files = Arc::new(files);
        let selected = Arc::new(AtomicUsize::new(0));
        let activation = Activation {
            toolkit: toolkit.to_owned(),
            files: files.clone(),
            selected: selected.clone(),
        };
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
        Some(Self {
            toolkit: toolkit.to_owned(),
            files,
            selected,
            adapter,
        })
    }

    /// Moves the selection (and keyboard focus) to another tree item.
    pub fn select(&mut self, index: usize) {
        self.selected.store(index, Ordering::Relaxed);
        let (toolkit, files) = (&self.toolkit, &self.files);
        let update = || tree_update(toolkit, files, index);
        #[cfg(windows)]
        if let Some(events) = self.adapter.update_if_active(update) {
            events.raise();
        }
        #[cfg(target_os = "linux")]
        self.adapter.update_if_active(update);
    }

    /// The toolkit window gained or lost focus (AT-SPI needs to know; Windows asks the window).
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
    fn tree_update_focuses_the_selection() {
        let files = workload::tree();
        let update = tree_update("test", &files, 3);
        assert_eq!(update.focus, item(3));
        assert_eq!(update.nodes.len(), files.len() + 3);
    }
}
