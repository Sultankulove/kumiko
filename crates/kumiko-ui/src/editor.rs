//! The editor seen from the workbench (spec, section 12: the editor is replaced at build
//! time). The workbench knows editors only through [`EditorProvider`]; the app plugs in the
//! implementation (`kumiko-editor-view`), so UI crates do not depend on each other.

use gpui::{AnyView, App, FocusHandle, Window};
use kumiko_core::CoreApi;
use kumiko_core::documents::Snapshot;
use tokio::runtime::Handle;

/// Colours (`0xRRGGBBAA`) and font of editors, from the theme and settings.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorTheme {
    pub background: u32,
    pub foreground: u32,
    pub gutter: u32,
    pub line_number: u32,
    pub line_number_active: u32,
    pub line_highlight: u32,
    pub selection: u32,
    pub caret: u32,
    pub find_match: u32,
    /// Syntax token styles: capture name (`keyword`, `function.method`, …), colour, bold,
    /// italic. A capture without its own style uses its dotted parent's.
    pub syntax: Vec<(String, u32, bool, bool)>,
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
}

/// What the status bar shows about an editor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditorStatus {
    /// 1-based line and column of the primary caret.
    pub line: usize,
    pub column: usize,
    pub carets: usize,
    /// The last failure (e.g. editing a read-only file).
    pub message: Option<String>,
}

/// Runs a command; `false` when it is not one of the provider's.
pub type RunCommand = Box<dyn Fn(&str, &mut App) -> bool>;
/// Puts the caret on a 1-based line.
pub type GotoLine = Box<dyn Fn(usize, &mut App)>;
/// Applies new colours and font.
pub type SetTheme = Box<dyn Fn(EditorTheme, &mut App)>;

/// One editor showing a document.
pub struct EditorHandle {
    pub view: AnyView,
    pub focus: FocusHandle,
    /// Runs one of the provider's commands; `false` when it is not one of them.
    pub run: RunCommand,
    /// Puts the caret on a 1-based line and scrolls to it.
    pub goto_line: GotoLine,
    pub status: Box<dyn Fn(&App) -> EditorStatus>,
    pub set_theme: SetTheme,
}

/// Creates editors.
pub trait EditorProvider: 'static {
    /// Commands an editor runs itself (`editor.*`, `edit.undo`, …); keys bound to them in an
    /// editor go straight to it.
    fn commands(&self) -> &'static [&'static str];

    fn open(
        &self,
        core: &CoreApi,
        runtime: &Handle,
        snapshot: &Snapshot,
        theme: EditorTheme,
        window: &mut Window,
        cx: &mut App,
    ) -> EditorHandle;
}
