//! Terminals seen from the workbench: like editors, the view comes from the app
//! (`kumiko-terminal-view`), so UI crates do not depend on each other.

use std::sync::Arc;

use gpui::{AnyView, App, FocusHandle, Window};
use kumiko_core::CoreApi;
use kumiko_core::terminals::Terminal;

/// One terminal session in the Terminal tool window.
pub struct TerminalHandle {
    pub view: AnyView,
    pub focus: FocusHandle,
}

/// How a terminal view looks and what it says when the shell ends.
pub struct TerminalOptions {
    pub font_family: String,
    pub font_size: f32,
    /// The line shown after the shell ended, from its exit code.
    pub exited: Box<dyn Fn(Option<i32>) -> String>,
}

/// Creates terminal views.
pub trait TerminalProvider: 'static {
    /// A view of a running terminal.
    fn open(
        &self,
        core: &CoreApi,
        terminal: Arc<Terminal>,
        options: TerminalOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> TerminalHandle;
}
