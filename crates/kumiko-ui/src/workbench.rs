//! The workbench window (spec, section 20) in the Classic UI arrangement of JetBrains IDEs:
//! menu bar, toolbar with the navigation bar, tool-window stripes, tool windows, editor tabs,
//! status bar. Geometry comes from [`LayoutState::regions`], the project tree and the tabs are
//! [`FileTree`] and [`Tabs`]; this module draws them and routes input to commands.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, FontWeight, HighlightStyle, KeyDownEvent,
    MouseButton, ScrollStrategy, SharedString, StyledText, UniformListScrollHandle, Window, div,
    prelude::*, px, rgba, uniform_list,
};
use kumiko_core::CoreApi;
use kumiko_core::config::keymap::{KeyChord, KeySequence, Resolution};
use kumiko_core::documents::{Encoding, LineEnding, Snapshot};
use kumiko_core::events::{Delivery, EventFilter};
use kumiko_core::plugin_host::PluginStatus;
use kumiko_core::protocol::events::FsChanged;
use kumiko_core::protocol::events::PluginState;
use kumiko_core::protocol::methods::fs::FileKind;
use kumiko_core::resources::ResourceKind;
use kumiko_core::service_api::FileSystem;
use kumiko_types::{CommandId, DocumentId, Uri};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::a11y::{AccessNode, AccessTree, Bridge};
use crate::commands::{NEEDS_ARGUMENTS, UiCommand};
use crate::editor::{EditorHandle, EditorProvider, EditorTheme};
use crate::keys;
use crate::layout::{LayoutState, Rect, Regions, Side, TAB_BAR_HEIGHT, ToolWindow};
use crate::menus::{self, CommandLookup, Menu};
use crate::palette::Palette;
use crate::problems::{self, Group, Problem, Severity};
use crate::quick_open::{self, Action, Item, Mode, QuickOpen};
use crate::state::{self, GroupState, WorkspaceState};
use crate::tabs::{Split, Tab, Tabs};
use crate::tree::{Entry, FileTree, Row};

#[cfg(windows)]
const UI_FONT: &str = "Segoe UI";
#[cfg(not(windows))]
const UI_FONT: &str = "Noto Sans";
#[cfg(windows)]
const MONOSPACE: &str = "Consolas";
#[cfg(not(windows))]
const MONOSPACE: &str = "DejaVu Sans Mono";
const UI_TEXT: f32 = 13.0;
const ROW_HEIGHT: f32 = 20.0;
/// Results shown at once in Quick Open.
const QUICK_ROWS: usize = 14;

/// Accessible node ids of the workbench regions; tree items and tabs get ranges of their own.
mod node {
    pub const WINDOW: u64 = 1;
    pub const MENU_BAR: u64 = 2;
    pub const TOOLBAR: u64 = 3;
    pub const PROJECT: u64 = 4;
    pub const EDITOR: u64 = 5;
    pub const BOTTOM: u64 = 6;
    pub const STATUS: u64 = 7;
    pub const TAB_LIST: u64 = 8;
    pub const MENUS: u64 = 100;
    pub const TABS: u64 = 1_000;
    pub const TREE_ITEMS: u64 = 1_000_000;
}

/// Where keyboard focus should go on the next frame (focus needs the window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusTarget {
    Project,
    Editor,
    Quick,
}

/// A file open in tabs: its document and how it is stored (for the status bar).
struct OpenFile {
    doc: DocumentId,
    format: String,
}

pub struct Workbench {
    core: CoreApi,
    runtime: Handle,
    locale: String,
    palette: Palette,
    layout: LayoutState,
    root_focus: FocusHandle,
    project_focus: FocusHandle,
    editor_focus: FocusHandle,
    focus_request: Option<FocusTarget>,
    /// Focus flags last published as context keys.
    published_focus: Option<(bool, bool)>,
    /// Chords typed so far of a multi-key binding.
    pending: Vec<KeyChord>,
    /// The main menu, rebuilt when it opens; the index of the open one.
    menus: Vec<Menu>,
    open_menu: Option<usize>,
    tree: FileTree,
    rows: Vec<Row>,
    tree_scroll: UniformListScrollHandle,
    split: Split,
    files: HashMap<Uri, OpenFile>,
    /// Editors by group and file: each group keeps its own carets and scroll.
    editors: HashMap<(usize, Uri), EditorHandle>,
    provider: Rc<dyn EditorProvider>,
    /// The last message for the status bar (e.g. a failed command).
    message: Option<String>,
    a11y: Option<Bridge>,
    /// The open Quick Open popup and its focus.
    quick_open: Option<QuickOpen>,
    quick_focus: FocusHandle,
    /// Where the workspace state is saved, once a folder is open.
    state_file: Option<std::path::PathBuf>,
    /// Files opened, most recent first (shown first in Quick Open).
    recent: Vec<Uri>,
    /// Problems grouped by source (Problems tool window).
    problems: Vec<Group>,
    /// The Output tool window: channel shown, its lines.
    output_channel: Option<String>,
    output_lines: Arc<Vec<SharedString>>,
    output_scroll: UniformListScrollHandle,
    /// Process start, until the first frame is drawn (then logged, section 23 budgets).
    started: Option<Instant>,
    /// The window has been shown (frames before that are not seen).
    shown: bool,
}

impl Workbench {
    pub fn new(
        core: CoreApi,
        runtime: Handle,
        requests: UnboundedReceiver<UiCommand>,
        started: Instant,
        provider: Rc<dyn EditorProvider>,
        cx: &mut Context<Self>,
    ) -> Self {
        let locale = core.locale();
        let palette = palette(&core);
        // Commands handled on the core's side arrive here, on the UI thread.
        cx.spawn(async move |this, cx| {
            let mut requests = requests;
            while let Some(command) = requests.recv().await {
                let applied = this.update(cx, |workbench, cx| workbench.apply(command, cx));
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut workbench = Self {
            core,
            runtime,
            locale,
            palette,
            layout: LayoutState::default(),
            root_focus: cx.focus_handle(),
            project_focus: cx.focus_handle(),
            editor_focus: cx.focus_handle(),
            focus_request: Some(FocusTarget::Project),
            published_focus: None,
            pending: Vec::new(),
            menus: Vec::new(),
            open_menu: None,
            tree: FileTree::default(),
            rows: Vec::new(),
            tree_scroll: UniformListScrollHandle::new(),
            split: Split::default(),
            files: HashMap::new(),
            editors: HashMap::new(),
            provider,
            message: None,
            a11y: None,
            started: Some(started),
            shown: false,
            quick_open: None,
            quick_focus: cx.focus_handle(),
            state_file: None,
            recent: Vec::new(),
            problems: Vec::new(),
            output_channel: None,
            output_lines: Arc::new(Vec::new()),
            output_scroll: UniformListScrollHandle::new(),
        };
        cx.on_app_quit(|workbench, _cx| {
            workbench.save_state();
            async {}
        })
        .detach();
        workbench.menus = workbench.build_menus();
        workbench.watch_events(cx);
        workbench.watch_output(cx);
        workbench.refresh_roots(cx);
        workbench.refresh_problems();
        workbench.refresh_output();
        workbench.publish_context();
        workbench
    }

    /// Hooks up accessibility once the window exists, before it is shown.
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // GPUI 0.2.2 panics in `window_handle()` on X11 (`unimplemented!`); the Linux
        // adapter needs no handle anyway.
        #[cfg(windows)]
        let handle = {
            use raw_window_handle::HasWindowHandle;
            window.window_handle().ok().map(|h| h.as_raw())
        };
        #[cfg(not(windows))]
        let handle = None;
        self.a11y = Bridge::attach(handle, self.access_tree(window));
        cx.observe_window_activation(window, |this, window, _cx| {
            let active = window.is_window_active();
            if let Some(a11y) = &mut this.a11y {
                a11y.window_focused(active);
            }
        })
        .detach();
        self.shown = true;
        // The first frame may already be drawn; focus lands on the next one.
        self.focus_request = Some(FocusTarget::Project);
        cx.notify();
    }

    fn t(&self, key: &str) -> String {
        self.core.resources().localize(key, &self.locale)
    }

    // ---- commands -------------------------------------------------------------------------

    fn apply(&mut self, command: UiCommand, cx: &mut Context<Self>) {
        match command {
            UiCommand::Toggle(window) => self.toggle_tool_window(window),
            UiCommand::OpenFile(uri) => self.open_file(uri, cx),
            UiCommand::QuickOpen(mode) => self.open_quick(mode),
            UiCommand::SelectTheme => {
                self.open_menu = None;
                self.quick_open = Some(QuickOpen::picker(Mode::Themes));
                self.refresh_quick();
                self.focus_request = Some(FocusTarget::Quick);
            }
            UiCommand::SetTheme(id) => self.set_theme(&id),
            UiCommand::Editor(id) => {
                self.run_in_editor(&id, cx);
                self.focus_request = Some(FocusTarget::Editor);
            }
            UiCommand::CloseEditor => self.close_active_tab(),
            UiCommand::Split(orientation) => self.split.split(orientation),
            UiCommand::Unsplit => self.split.unsplit(),
            UiCommand::NextEditor => self.split.tabs_mut().cycle(1),
            UiCommand::PreviousEditor => self.split.tabs_mut().cycle(-1),
            UiCommand::Quit => cx.quit(),
        }
        self.publish_context();
        self.save_state();
        cx.notify();
    }

    /// As in JetBrains IDEs: a hidden tool window opens and takes focus; an open one takes
    /// focus; a focused one hides.
    fn toggle_tool_window(&mut self, window: ToolWindow) {
        let open = self.layout.is_open(window);
        let focused = window == ToolWindow::Project && self.published_focus.is_some_and(|f| f.0);
        if !open {
            self.layout.toggle(window);
            if window == ToolWindow::Project {
                self.focus_request = Some(FocusTarget::Project);
            }
        } else if window == ToolWindow::Project && !focused {
            self.focus_request = Some(FocusTarget::Project);
        } else {
            self.layout.toggle(window);
            self.focus_request = Some(FocusTarget::Editor);
        }
    }

    /// Context keys for when-expressions: visible tool windows, open editors.
    fn publish_context(&self) {
        let context = self.core.commands().context();
        for window in ToolWindow::ALL {
            let name = window.title_key().trim_start_matches("toolWindow.");
            context.set(
                format!("workbench.{name}.visible"),
                Value::Bool(self.layout.is_open(window)),
            );
        }
        context.set("activeEditorCount", Value::from(self.split.tabs().len()));
    }

    /// Runs a command on the core's runtime; a failure shows in the status bar.
    fn execute(&mut self, id: CommandId, args: Vec<Value>, cx: &mut Context<Self>) {
        self.open_menu = None;
        let core = self.core.clone();
        let task = self
            .runtime
            .spawn(async move { core.commands().execute(&id, args).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = task.await {
                let _ = this.update(cx, |workbench, cx| {
                    workbench.message = Some(error.to_string());
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn execute_id(&mut self, id: &str, args: Vec<Value>, cx: &mut Context<Self>) {
        if let Ok(id) = CommandId::new(id) {
            self.execute(id, args, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        tracing::debug!(key = ?stroke.key, key_char = ?stroke.key_char, "raw key");
        let Some(chord) = keys::chord(
            modifiers.control,
            modifiers.alt,
            modifiers.shift,
            modifiers.platform,
            &stroke.key,
        ) else {
            return;
        };
        if chord.key == "escape" && self.open_menu.is_some() {
            self.open_menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        self.pending.push(chord);
        let pressed = KeySequence(self.pending.clone());
        let keymap = self.core.config().keymap();
        let resolution = keymap.resolve(&pressed, self.core.commands().context());
        tracing::debug!(keys = %keys::label(&pressed), ?resolution, "key");
        match resolution {
            Resolution::Command(id) => {
                self.pending.clear();
                cx.stop_propagation();
                // Editor keys go straight to the focused editor: no round trip through the
                // core runtime on every keystroke.
                let editor_command = self.provider.commands().contains(&id.as_str());
                let focused = self
                    .active_editor()
                    .is_some_and(|e| e.focus.is_focused(window));
                if !(editor_command && focused && self.run_in_editor(id.as_str(), cx)) {
                    self.execute(id, Vec::new(), cx);
                }
                cx.notify();
            }
            Resolution::Pending => cx.stop_propagation(),
            Resolution::None => self.pending.clear(),
        }
    }

    // ---- project tree ---------------------------------------------------------------------

    /// Subscribes before the roots are first read, so no folder added meanwhile is missed.
    fn watch_events(&self, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let topics = [
            "fs.changed",
            "workspace.folderAdded",
            "workspace.folderRemoved",
            "config.*",
            "plugin.*",
        ];
        let mut events = core.events().subscribe(EventFilter::topics(topics));
        cx.spawn(async move |this, cx| {
            while let Some(delivery) = events.recv().await {
                if matches!(delivery, Delivery::Resync(_)) {
                    events = core.events().subscribe(EventFilter::topics(topics));
                }
                let handled = this.update(cx, |workbench, cx| workbench.on_event(delivery, cx));
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_event(&mut self, delivery: Delivery, cx: &mut Context<Self>) {
        let event = match delivery {
            Delivery::Event(event) => event,
            Delivery::Resync(_) => {
                // Events were lost: reload everything shown.
                self.refresh_problems();
                self.refresh_roots(cx);
                let dirs: Vec<Uri> = self.tree.loaded_dirs().cloned().collect();
                for dir in dirs {
                    self.load_dir(dir, cx);
                }
                return;
            }
        };
        if event.topic.starts_with("config.") || event.topic.starts_with("plugin.") {
            if event.topic.starts_with("config.") {
                self.reload_settings(cx);
            }
            self.refresh_problems();
            cx.notify();
        } else if let Some(Ok(changed)) = event.decode::<FsChanged>() {
            let mut dirs: Vec<Uri> = changed
                .changes
                .iter()
                .filter_map(|c| {
                    let parent = c.uri.to_file_path()?.parent()?.to_path_buf();
                    Uri::from_file_path(parent).ok()
                })
                .filter(|dir| self.tree.is_loaded(dir))
                .collect();
            dirs.sort();
            dirs.dedup();
            for dir in dirs {
                self.load_dir(dir, cx);
            }
        } else {
            self.refresh_roots(cx);
        }
    }

    fn refresh_roots(&mut self, cx: &mut Context<Self>) {
        let roots = self
            .core
            .project()
            .folders()
            .into_iter()
            .map(|f| Entry {
                uri: f.uri,
                name: f.name,
                dir: true,
            })
            .collect();
        for dir in self.tree.set_roots(roots) {
            self.load_dir(dir, cx);
        }
        if self.state_file.is_none() && !self.core.project().folders().is_empty() {
            self.restore_state(cx);
        }
        cx.notify();
    }

    /// Reads a directory on the core's runtime, leaving out what `files.exclude` hides.
    fn load_dir(&self, dir: Uri, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let task = self.runtime.spawn(async move {
            let entries = core.fs().read_dir(&dir).await?;
            let base = dir.to_file_path();
            let entries: Vec<Entry> = entries
                .into_iter()
                .filter_map(|e| {
                    let path = base.as_ref()?.join(&e.name);
                    if core.project().is_excluded(&path) {
                        return None;
                    }
                    Some(Entry {
                        uri: Uri::from_file_path(&path).ok()?,
                        name: e.name,
                        dir: e.kind == FileKind::Directory,
                    })
                })
                .collect();
            Ok::<_, kumiko_core::service_api::FsError>((dir, entries))
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(Ok((dir, entries))) => {
                let _ = this.update(cx, |workbench, cx| {
                    workbench.tree.set_children(&dir, entries);
                    cx.notify();
                });
            }
            Ok(Err(error)) => tracing::warn!("cannot list a directory: {error}"),
            Err(error) => tracing::warn!("listing a directory failed: {error}"),
        })
        .detach();
    }

    fn tree_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.tree.move_selection(1),
            "up" => self.tree.move_selection(-1),
            "pagedown" => self.tree.move_selection(20),
            "pageup" => self.tree.move_selection(-20),
            "home" => self.tree.move_selection(isize::MIN / 2),
            "end" => self.tree.move_selection(isize::MAX / 2),
            "right" => {
                if let Some(dir) = self.tree.right() {
                    self.load_dir(dir, cx);
                }
            }
            "left" => self.tree.left(),
            // As in JetBrains IDEs: Escape leaves a tool window for the editor.
            "escape" => self.focus_request = Some(FocusTarget::Editor),
            "enter" => self.activate_selected(cx),
            _ => return,
        }
        cx.stop_propagation();
        self.reveal_selection();
        cx.notify();
    }

    /// Enter or double click: a directory expands or collapses, a file opens.
    fn activate_selected(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.tree.selected_row() else {
            return;
        };
        if row.dir {
            if let Some(dir) = self.tree.toggle(&row.uri) {
                self.load_dir(dir, cx);
            }
        } else {
            self.execute_id(
                "workbench.openFile",
                vec![Value::from(row.uri.as_str())],
                cx,
            );
        }
    }

    fn reveal_selection(&self) {
        let rows = self.tree.rows();
        if let Some(index) = self
            .tree
            .selected()
            .and_then(|s| rows.iter().position(|r| &r.uri == s))
        {
            self.tree_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    // ---- editors --------------------------------------------------------------------------

    fn open_file(&mut self, uri: Uri, cx: &mut Context<Self>) {
        let title = file_name(&uri);
        self.focus_request = Some(FocusTarget::Editor);
        if self.files.contains_key(&uri) {
            self.remember(&uri);
            self.split.tabs_mut().open(uri, title);
            return;
        }
        let core = self.core.clone();
        let opened = uri.clone();
        let task = self
            .runtime
            .spawn(async move { core.documents().open(&opened, None).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |workbench, cx| {
                match result {
                    Ok(Ok(snapshot)) => {
                        workbench.remember(&uri);
                        workbench
                            .files
                            .insert(uri.clone(), OpenFile::new(&snapshot));
                        workbench.split.tabs_mut().open(uri, title);
                        workbench.save_state();
                        // The editor exists only now: focus it on the next frame.
                        workbench.focus_request = Some(FocusTarget::Editor);
                        workbench.publish_context();
                    }
                    Ok(Err(error)) => {
                        let text = workbench
                            .t("editor.cannotOpen")
                            .replace("{file}", &title)
                            .replace("{error}", &error.to_string());
                        workbench.message = Some(text);
                    }
                    Err(error) => workbench.message = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Closes the active tab; the document closes once no group shows the file.
    fn close_active_tab(&mut self) {
        let closed = self.split.close_active();
        self.prune_editors();
        if let Some(tab) = closed
            && !self.split.holds(&tab.uri)
            && let Some(file) = self.files.remove(&tab.uri)
            && let Err(error) = self.core.documents().close(file.doc)
        {
            tracing::warn!("closing {}: {error}", tab.uri);
        }
        if self.split.tabs().len() == 0 {
            self.focus_request = Some(FocusTarget::Project);
        }
    }

    /// Drops editors whose tab is gone (closed, or its group merged away).
    fn prune_editors(&mut self) {
        let groups = self.split.groups();
        self.editors.retain(|(g, uri), _| {
            groups
                .get(*g)
                .is_some_and(|tabs| tabs.position(uri).is_some())
        });
    }

    /// Creates the editors of the groups' active tabs once their documents are open.
    fn ensure_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prune_editors();
        let wanted: Vec<(usize, Uri)> = self
            .split
            .groups()
            .iter()
            .enumerate()
            .filter_map(|(g, tabs)| tabs.active().map(|t| (g, t.uri.clone())))
            .filter(|key| !self.editors.contains_key(key))
            .collect();
        for key in wanted {
            let Some(file) = self.files.get(&key.1) else {
                continue;
            };
            let Ok(snapshot) = self.core.documents().snapshot(file.doc) else {
                continue;
            };
            let theme = self.editor_theme();
            let handle =
                self.provider
                    .open(&self.core, &self.runtime, &snapshot, theme, window, cx);
            self.editors.insert(key, handle);
        }
    }

    /// The editor of the active group's active tab.
    fn active_editor(&self) -> Option<&EditorHandle> {
        let tab = self.split.tabs().active()?;
        self.editors
            .get(&(self.split.active_group(), tab.uri.clone()))
    }

    /// Editor colours from the theme, font from the settings.
    fn editor_theme(&self) -> EditorTheme {
        let p = &self.palette;
        let config = self.core.config();
        let family = config
            .get("editor.fontFamily", None)
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|f| !f.is_empty() && f != "monospace")
            .unwrap_or_else(|| MONOSPACE.to_owned());
        let font_size = config
            .get("editor.fontSize", None)
            .and_then(|v| v.as_f64())
            .unwrap_or(14.0) as f32;
        EditorTheme {
            background: p.editor,
            foreground: p.editor_foreground,
            gutter: p.editor_gutter,
            line_number: p.line_number,
            line_number_active: p.line_number_active,
            line_highlight: p.line_highlight,
            selection: p.editor_selection,
            caret: p.caret,
            syntax: p
                .syntax
                .iter()
                .map(|s| (s.capture.clone(), s.color, s.bold, s.italic))
                .collect(),
            font_family: family,
            font_size,
            line_height: (font_size * 1.4).round(),
        }
    }

    /// Runs an editor command in the active editor; `false` without one.
    fn run_in_editor(&self, command: &str, cx: &mut Context<Self>) -> bool {
        match self.active_editor() {
            Some(editor) => (editor.run)(command, cx),
            None => false,
        }
    }

    // ---- quick open -----------------------------------------------------------------------

    fn open_quick(&mut self, mode: Mode) {
        self.open_menu = None;
        self.quick_open = Some(QuickOpen::new(mode));
        self.refresh_quick();
        self.focus_request = Some(FocusTarget::Quick);
    }

    fn close_quick(&mut self) {
        self.quick_open = None;
        self.focus_request = Some(if self.split.tabs().len() > 0 {
            FocusTarget::Editor
        } else {
            FocusTarget::Project
        });
    }

    fn remember(&mut self, uri: &Uri) {
        self.recent.retain(|u| u != uri);
        self.recent.insert(0, uri.clone());
        self.recent.truncate(50);
    }

    /// Recomputes the results for the current input.
    fn refresh_quick(&mut self) {
        let Some(quick) = &self.quick_open else {
            return;
        };
        let (mode, query) = (quick.mode(), quick.query().to_owned());
        let items = match mode {
            Mode::Files => self.file_items(&query),
            Mode::Commands => self.command_items(&query),
            Mode::Line => self.line_items(&query),
            Mode::Symbols => vec![self.note("quickOpen.symbolsUnavailable")],
            Mode::Themes => self.theme_items(&query),
        };
        let items = if items.is_empty() {
            vec![self.note("quickOpen.noResults")]
        } else {
            items
        };
        if let Some(quick) = &mut self.quick_open {
            quick.set_items(items);
        }
    }

    fn note(&self, key: &str) -> Item {
        Item {
            label: self.t(key),
            highlights: Vec::new(),
            detail: String::new(),
            keys: None,
            action: Action::None,
        }
    }

    /// Files from the core's index; with an empty query, recently opened files first.
    fn file_items(&self, query: &str) -> Vec<Item> {
        const LIMIT: usize = 50;
        let item = |uri: Uri, relative: &str, highlights: Vec<u32>, detail: String| Item {
            label: relative.rsplit('/').next().unwrap_or(relative).to_owned(),
            highlights,
            detail,
            keys: None,
            action: Action::OpenFile(uri),
        };
        let folder_of = |relative: &str| {
            relative
                .rsplit_once('/')
                .map_or_else(String::new, |(dir, _)| dir.to_owned())
        };
        let index = self.core.file_index();
        if query.is_empty() {
            let recent_label = self.t("quickOpen.recent");
            let mut items: Vec<Item> = self
                .recent
                .iter()
                .map(|uri| {
                    item(
                        uri.clone(),
                        &file_name(uri),
                        Vec::new(),
                        recent_label.clone(),
                    )
                })
                .collect();
            for found in index.find("", LIMIT) {
                if !self.recent.contains(&found.file.uri) {
                    let detail = folder_of(&found.file.relative);
                    items.push(item(
                        found.file.uri,
                        &found.file.relative,
                        Vec::new(),
                        detail,
                    ));
                }
            }
            items.truncate(LIMIT);
            return items;
        }
        index
            .find(query, LIMIT)
            .into_iter()
            .map(|found| {
                let name = found.file.name().to_owned();
                let highlights =
                    quick_open::name_highlights(&found.file.relative, &name, &found.indices);
                let detail = folder_of(&found.file.relative);
                item(found.file.uri, &found.file.relative, highlights, detail)
            })
            .collect()
    }

    /// Enabled commands, by their localized titles.
    fn command_items(&self, query: &str) -> Vec<Item> {
        let lookup = self.lookup();
        let commands: Vec<(CommandId, String)> = self
            .core
            .commands()
            .list()
            .into_iter()
            .filter(|c| self.core.commands().is_enabled(&c.id))
            .filter(|c| !NEEDS_ARGUMENTS.contains(&c.id.as_str()))
            .filter_map(|c| Some((c.id.clone(), lookup.title(&c.id)?)))
            .collect();
        let ranked: Vec<(usize, Vec<u32>)> = if query.is_empty() {
            let mut all: Vec<usize> = (0..commands.len()).collect();
            all.sort_by(|&a, &b| commands[a].1.cmp(&commands[b].1));
            all.into_iter().map(|i| (i, Vec::new())).collect()
        } else {
            kumiko_core::search::rank(
                query,
                commands.iter().map(|(_, title)| title.as_str()),
                100,
                false,
            )
            .into_iter()
            .map(|(i, _, highlights)| (i, highlights))
            .collect()
        };
        ranked
            .into_iter()
            .map(|(i, highlights)| {
                let (id, title) = &commands[i];
                Item {
                    label: title.clone(),
                    highlights,
                    detail: id.to_string(),
                    keys: lookup.keys(id),
                    action: Action::Command(id.clone()),
                }
            })
            .collect()
    }

    fn line_items(&self, query: &str) -> Vec<Item> {
        let Some(file) = self
            .split
            .tabs()
            .active()
            .and_then(|t| self.files.get(&t.uri))
        else {
            return vec![self.note("quickOpen.noFile")];
        };
        match quick_open::parse_line(query) {
            Some(line) => {
                let lines = self
                    .core
                    .documents()
                    .snapshot(file.doc)
                    .map_or(1, |s| s.text.len_lines());
                let line = line.min(lines.max(1));
                vec![Item {
                    label: self
                        .t("quickOpen.gotoLine")
                        .replace("{line}", &line.to_string()),
                    highlights: Vec::new(),
                    detail: String::new(),
                    keys: None,
                    action: Action::GotoLine(line),
                }]
            }
            None => vec![self.note("quickOpen.lineHint")],
        }
    }

    /// Installed colour themes by name; the current one says so.
    fn theme_items(&self, query: &str) -> Vec<Item> {
        let current = self
            .core
            .config()
            .get("workbench.theme", None)
            .and_then(|v| v.as_str().map(str::to_owned));
        let themes: Vec<(String, String)> = self
            .core
            .resources()
            .list(ResourceKind::Theme)
            .into_iter()
            .map(|package| {
                let name = package.name.as_deref().unwrap_or(&package.id).to_owned();
                (package.id, self.core.resources().label(&name, &self.locale))
            })
            .collect();
        let ranked: Vec<(usize, Vec<u32>)> = if query.is_empty() {
            (0..themes.len()).map(|i| (i, Vec::new())).collect()
        } else {
            kumiko_core::search::rank(query, themes.iter().map(|(_, n)| n.as_str()), 50, false)
                .into_iter()
                .map(|(i, _, highlights)| (i, highlights))
                .collect()
        };
        ranked
            .into_iter()
            .map(|(i, highlights)| {
                let (id, name) = &themes[i];
                Item {
                    label: name.clone(),
                    highlights,
                    detail: if current.as_deref() == Some(id.as_str()) {
                        self.t("quickOpen.currentTheme")
                    } else {
                        String::new()
                    },
                    keys: None,
                    action: Action::SetTheme(id.clone()),
                }
            })
            .collect()
    }

    /// Writes the theme to the user settings; the change comes back as `config.changed`.
    fn set_theme(&mut self, id: &str) {
        let written = self.core.config().update(
            "workbench.theme",
            Value::from(id),
            kumiko_core::protocol::methods::config::ConfigTarget::User,
        );
        if let Err(error) = written {
            self.message = Some(error.to_string());
        }
    }

    /// Settings the workbench shows changed: colours, language, menus.
    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        self.palette = palette(&self.core);
        self.locale = self.core.locale();
        self.menus = self.build_menus();
        let theme = self.editor_theme();
        for editor in self.editors.values() {
            (editor.set_theme)(theme.clone(), cx);
        }
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(action) = self
            .quick_open
            .as_ref()
            .and_then(|q| q.items.get(index))
            .map(|i| i.action.clone())
        else {
            return;
        };
        if action == Action::None {
            return;
        }
        self.close_quick();
        match action {
            Action::OpenFile(uri) => {
                self.execute_id("workbench.openFile", vec![Value::from(uri.as_str())], cx)
            }
            Action::Command(id) => self.execute(id, Vec::new(), cx),
            Action::SetTheme(id) => {
                self.execute_id("workbench.setTheme", vec![Value::from(id)], cx)
            }
            Action::GotoLine(line) => {
                if let Some(editor) = self.active_editor() {
                    (editor.goto_line)(line, cx);
                }
                self.focus_request = Some(FocusTarget::Editor);
            }
            Action::None => {}
        }
        cx.notify();
    }

    fn quick_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(quick) = &mut self.quick_open else {
            return;
        };
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        let edited = match stroke.key.as_str() {
            "escape" => {
                self.close_quick();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            "enter" => {
                let selected = quick.selected;
                cx.stop_propagation();
                self.choose(selected, cx);
                return;
            }
            "down" => {
                quick.move_selection(1);
                false
            }
            "up" => {
                quick.move_selection(-1);
                false
            }
            "pagedown" => {
                quick.move_selection(10);
                false
            }
            "pageup" => {
                quick.move_selection(-10);
                false
            }
            "backspace" => {
                quick.input.backspace();
                true
            }
            "delete" => {
                quick.input.delete();
                true
            }
            "left" => {
                quick.input.left();
                false
            }
            "right" => {
                quick.input.right();
                false
            }
            "home" => {
                quick.input.home();
                false
            }
            "end" => {
                quick.input.end();
                false
            }
            _ => {
                // Text is what the key types; shortcuts go on to the keymap.
                let shortcut = modifiers.control || modifiers.platform || modifiers.alt;
                match stroke.key_char.as_deref() {
                    Some(text) if !shortcut => {
                        quick.input.insert(text);
                        true
                    }
                    _ => return,
                }
            }
        };
        cx.stop_propagation();
        if edited {
            self.refresh_quick();
        }
        cx.notify();
    }

    // ---- problems and output --------------------------------------------------------------

    /// Everything the core reports as a problem, now.
    fn collect_problems(&self) -> Vec<Problem> {
        let file = |path: &std::path::Path| Uri::from_file_path(path).ok();
        let name = |path: &std::path::Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let mut problems = Vec::new();
        for problem in self.core.config().problems() {
            let path = std::path::Path::new(&problem.source);
            let location = path.is_absolute().then(|| file(path)).flatten();
            let message = match &problem.key {
                Some(key) => format!("{key}: {}", problem.message),
                None => problem.message.clone(),
            };
            problems.push(Problem {
                severity: Severity::Warning,
                source: if location.is_some() {
                    name(path)
                } else {
                    problem.source.clone()
                },
                message,
                location,
            });
        }
        for problem in self.core.resources().problems() {
            problems.push(Problem {
                severity: Severity::Warning,
                source: name(&problem.path),
                message: problem.message,
                location: file(&problem.path),
            });
        }
        let registry = self.core.plugins();
        for message in registry.problems() {
            problems.push(Problem {
                severity: Severity::Error,
                source: self.t("problems.plugins"),
                message: message.clone(),
                location: None,
            });
        }
        let host = self.core.plugin_host();
        for entry in registry.entries() {
            let id = entry.manifest().id();
            let manifest = file(&entry.plugin.dir.join("plugin.toml"));
            if let PluginStatus::Incompatible(reasons) = &entry.status {
                for reason in reasons {
                    problems.push(Problem {
                        severity: Severity::Warning,
                        source: id.to_string(),
                        message: reason.clone(),
                        location: manifest.clone(),
                    });
                }
            }
            if host.state(id) == Some(PluginState::Failed) {
                problems.push(Problem {
                    severity: Severity::Error,
                    source: id.to_string(),
                    message: host.reason(id).unwrap_or_else(|| self.t("problems.failed")),
                    location: None,
                });
            }
        }
        problems
    }

    fn refresh_problems(&mut self) {
        self.problems = problems::group(self.collect_problems());
    }

    /// Follows the output channels, at most every 100 ms however fast they grow.
    fn watch_output(&self, cx: &mut Context<Self>) {
        let mut changes = self.core.output().watch();
        cx.spawn(async move |this, cx| {
            while changes.changed().await.is_ok() {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                changes.mark_unchanged();
                let updated = this.update(cx, |workbench, cx| {
                    workbench.refresh_output();
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_output(&mut self) {
        let channels = self.core.output().channels();
        if self
            .output_channel
            .as_ref()
            .is_none_or(|c| !channels.contains(c))
        {
            self.output_channel = channels.first().cloned();
        }
        let lines: Vec<SharedString> = self
            .output_channel
            .as_deref()
            .map(|c| self.core.output().lines(c))
            .unwrap_or_default()
            .into_iter()
            .map(SharedString::from)
            .collect();
        let grew = lines.len() != self.output_lines.len();
        self.output_lines = Arc::new(lines);
        if grew && !self.output_lines.is_empty() {
            self.output_scroll
                .scroll_to_item(self.output_lines.len() - 1, ScrollStrategy::Bottom);
        }
    }

    fn channel_title(&self, channel: &str) -> String {
        let key = format!("output.channel.{channel}");
        let title = self.t(&key);
        if title == key {
            channel.to_owned()
        } else {
            title
        }
    }

    // ---- workspace state --------------------------------------------------------------------

    /// Restores the workspace's saved layout and tabs once its first folder is known.
    fn restore_state(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.core.project().folders().into_iter().next() else {
            return;
        };
        let path = state::path(&self.core.dirs().workspaces_state_dir(), &folder.uri);
        let saved = state::load(&path);
        self.state_file = Some(path);
        self.layout = saved.layout;
        self.recent = saved.recent;
        let groups = saved
            .groups
            .into_iter()
            .map(|g| {
                let tabs = g
                    .files
                    .into_iter()
                    .map(|uri| Tab {
                        title: file_name(&uri),
                        uri,
                    })
                    .collect();
                (tabs, g.active)
            })
            .collect();
        self.split = Split::restore(groups, saved.active_group, saved.orientation);
        let files: Vec<Uri> = self
            .split
            .groups()
            .iter()
            .flat_map(|g| g.iter().map(|t| t.uri.clone()))
            .collect();
        for uri in files {
            self.load_document(uri, cx);
        }
        self.publish_context();
        cx.notify();
    }

    /// Opens a restored tab's document; a file that is gone loses its tabs.
    fn load_document(&self, uri: Uri, cx: &mut Context<Self>) {
        if self.files.contains_key(&uri) {
            return;
        }
        let core = self.core.clone();
        let opened = uri.clone();
        let task = self
            .runtime
            .spawn(async move { core.documents().open(&opened, None).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |workbench, cx| {
                match result {
                    Ok(Ok(snapshot)) => {
                        workbench.files.insert(uri, OpenFile::new(&snapshot));
                    }
                    _ => {
                        workbench.split.remove_file(&uri);
                        workbench.save_state();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Writes the workspace state (after changes and on quit); failures are only logged.
    fn save_state(&self) {
        let Some(path) = &self.state_file else {
            return;
        };
        let (groups, active_group, orientation) = self.split.to_state();
        let saved = WorkspaceState {
            layout: self.layout.clone(),
            groups: groups
                .into_iter()
                .map(|(files, active)| GroupState { files, active })
                .collect(),
            active_group,
            orientation,
            recent: self.recent.clone(),
        };
        if let Err(error) = state::save(path, &saved) {
            tracing::warn!(
                "cannot save the workspace state to {}: {error}",
                path.display()
            );
        }
    }

    // ---- menus ----------------------------------------------------------------------------

    fn build_menus(&self) -> Vec<Menu> {
        let registry = self.core.plugins();
        let context = self.core.commands().context();
        let contributions: Vec<(String, CommandId)> = registry
            .menus()
            .filter(|(_, m)| {
                m.when.as_deref().is_none_or(|w| {
                    kumiko_core::service_api::when::WhenExpr::parse(w)
                        .is_ok_and(|w| context.eval(&w))
                })
            })
            .map(|(_, m)| (m.location.clone(), m.command.clone()))
            .collect();
        menus::main_menu(&self.lookup(), &|key| self.t(key), &contributions)
    }

    fn lookup(&self) -> Lookup<'_> {
        Lookup {
            core: &self.core,
            locale: &self.locale,
            keymap: self.core.config().keymap(),
        }
    }

    fn toggle_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.open_menu == Some(index) {
            self.open_menu = None;
        } else {
            self.menus = self.build_menus();
            self.open_menu = Some(index);
        }
        cx.notify();
    }

    // ---- accessibility --------------------------------------------------------------------

    fn access_tree(&self, window: &Window) -> AccessTree {
        use accesskit::Role;
        let node = |id, role, label: String, children: Vec<u64>| AccessNode {
            id,
            role,
            label,
            children,
        };
        let mut regions = vec![node::MENU_BAR, node::TOOLBAR];
        if self.layout.left.is_some() {
            regions.push(node::PROJECT);
        }
        regions.push(node::TAB_LIST);
        regions.push(node::EDITOR);
        if self.layout.bottom.is_some() {
            regions.push(node::BOTTOM);
        }
        regions.push(node::STATUS);
        let menu_ids: Vec<u64> = (0..self.menus.len() as u64)
            .map(|i| node::MENUS + i)
            .collect();
        let tab_ids: Vec<u64> = (0..self.split.tabs().len() as u64)
            .map(|i| node::TABS + i)
            .collect();
        let editor_label = self
            .split
            .tabs()
            .active()
            .map_or_else(|| self.t("editor.empty"), |t| t.title.clone());
        let mut nodes = vec![
            node(node::WINDOW, Role::Window, self.t("app.name"), regions),
            node(node::MENU_BAR, Role::MenuBar, String::new(), menu_ids),
            node(
                node::TOOLBAR,
                Role::Toolbar,
                self.navigation().join(" / "),
                vec![],
            ),
            node(node::TAB_LIST, Role::TabList, String::new(), tab_ids),
            node(node::EDITOR, Role::Document, editor_label, vec![]),
            node(
                node::STATUS,
                Role::Status,
                self.message.clone().unwrap_or_default(),
                vec![],
            ),
        ];
        let mut focus = node::EDITOR;
        if let Some(tool) = self.layout.left {
            let items: Vec<u64> = (0..self.rows.len() as u64)
                .map(|i| node::TREE_ITEMS + i)
                .collect();
            nodes.push(node(
                node::PROJECT,
                Role::Tree,
                self.t(tool.title_key()),
                items,
            ));
            let selected = self.tree.selected();
            let tree_focused = self.project_focus.is_focused(window);
            for (i, row) in self.rows.iter().enumerate() {
                let id = node::TREE_ITEMS + i as u64;
                nodes.push(node(id, Role::TreeItem, row.name.clone(), vec![]));
                if tree_focused && Some(&row.uri) == selected {
                    focus = id;
                }
            }
        }
        if let Some(tool) = self.layout.bottom {
            nodes.push(node(
                node::BOTTOM,
                Role::Pane,
                self.t(tool.title_key()),
                vec![],
            ));
        }
        for (i, menu) in self.menus.iter().enumerate() {
            nodes.push(node(
                node::MENUS + i as u64,
                Role::MenuItem,
                menu.title.clone(),
                vec![],
            ));
        }
        for (i, tab) in self.split.tabs().iter().enumerate() {
            nodes.push(node(
                node::TABS + i as u64,
                Role::Tab,
                tab.title.clone(),
                vec![],
            ));
        }
        AccessTree { nodes, focus }
    }

    /// The navigation bar: the active file's path from its workspace folder.
    fn navigation(&self) -> Vec<String> {
        let folders = self.core.project().folders();
        let Some(tab) = self.split.tabs().active() else {
            return folders
                .first()
                .map(|f| vec![f.name.clone()])
                .unwrap_or_default();
        };
        let Some(path) = tab.uri.to_file_path() else {
            return vec![tab.title.clone()];
        };
        match self.core.project().folder_of(&path) {
            Some(folder) => {
                let root = folder.uri.to_file_path().unwrap_or_default();
                let relative = path.strip_prefix(&root).unwrap_or(&path);
                std::iter::once(folder.name)
                    .chain(relative.iter().map(|c| c.to_string_lossy().into_owned()))
                    .collect()
            }
            None => vec![tab.title.clone()],
        }
    }
}

impl OpenFile {
    fn new(snapshot: &Snapshot) -> Self {
        let mut lines: Vec<SharedString> = snapshot
            .text
            .lines()
            .map(|line| {
                let text = line.to_string();
                SharedString::from(text.trim_end_matches(['\n', '\r']).to_owned())
            })
            .collect();
        // A trailing newline yields an empty last line; editors do not show it as a line.
        if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let encoding = match snapshot.format.encoding {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Latin1 => "ISO-8859-1",
        };
        let ending = match snapshot.format.line_ending {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
        };
        Self {
            doc: snapshot.id,
            format: format!("{ending}   {encoding}"),
        }
    }
}

fn file_name(uri: &Uri) -> String {
    uri.to_file_path()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| uri.to_string())
}

/// Command titles, state and bindings for the menu.
struct Lookup<'a> {
    core: &'a CoreApi,
    locale: &'a str,
    keymap: kumiko_core::config::Keymap,
}

impl CommandLookup for Lookup<'_> {
    fn title(&self, command: &CommandId) -> Option<String> {
        let info = self
            .core
            .commands()
            .list()
            .into_iter()
            .find(|c| &c.id == command)?;
        let title = info.title.unwrap_or_else(|| command.to_string());
        Some(self.core.resources().label(&title, self.locale))
    }

    fn enabled(&self, command: &CommandId) -> bool {
        self.core.commands().is_enabled(command)
    }

    fn keys(&self, command: &CommandId) -> Option<String> {
        keys::binding_label(&self.keymap, command)
    }
}

fn palette(core: &CoreApi) -> Palette {
    let wanted = core
        .config()
        .get("workbench.theme", None)
        .and_then(|v| v.as_str().map(str::to_owned));
    let theme = wanted
        .as_deref()
        .and_then(|id| core.resources().theme(id))
        .or_else(|| core.resources().theme("kumiko.theme.darcula"));
    theme.map_or_else(Palette::fallback, |theme| Palette::from_theme(&theme))
}

/// An absolutely placed box for a region.
fn place(rect: Rect) -> gpui::Div {
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .overflow_hidden()
}

// ---- rendering ----------------------------------------------------------------------------

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_editors(window, cx);
        if self.shown
            && let Some(started) = self.started.take()
        {
            tracing::info!(
                "first frame {} ms after start",
                started.elapsed().as_millis()
            );
        }
        if let Some(target) = self.focus_request.take() {
            match target {
                FocusTarget::Project if self.layout.left.is_some() => {
                    self.project_focus.focus(window)
                }
                FocusTarget::Quick if self.quick_open.is_some() => self.quick_focus.focus(window),
                _ => match self.active_editor() {
                    Some(editor) => editor.focus.focus(window),
                    None => self.editor_focus.focus(window),
                },
            }
        }
        let focus = (
            self.project_focus.is_focused(window),
            self.active_editor()
                .is_some_and(|e| e.focus.is_focused(window)),
        );
        if self.published_focus != Some(focus) {
            self.published_focus = Some(focus);
            let context = self.core.commands().context();
            context.set("projectFocus", Value::Bool(focus.0));
            context.set("editorFocus", Value::Bool(focus.1));
        }
        self.rows = self.tree.rows();

        let size = window.viewport_size();
        let regions = self
            .layout
            .regions(f32::from(size.width), f32::from(size.height));
        let tree = self.access_tree(window);
        if let Some(a11y) = &mut self.a11y {
            a11y.update(tree);
        }
        let p = self.palette.clone();

        let mut root = div()
            .id("workbench")
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .bg(rgba(p.window))
            .text_color(rgba(p.foreground))
            .font_family(UI_FONT)
            .text_size(px(UI_TEXT))
            .child(self.menu_bar(regions.menu_bar, cx))
            .child(self.toolbar(regions.toolbar))
            .child(self.stripe(Side::Left, regions.left_stripe, cx))
            .child(
                place(regions.right_stripe)
                    .bg(rgba(p.stripe))
                    .border_l_1()
                    .border_color(rgba(p.border)),
            )
            .child(self.stripe(Side::Bottom, regions.bottom_stripe, cx))
            .child(self.editor_area(regions.editor, cx))
            .child(self.status_bar(regions.status_bar, cx));
        if let (Some(rect), Some(tool)) = (regions.left_panel, self.layout.left) {
            root = root.child(self.tool_window(tool, rect, cx));
        }
        if let (Some(rect), Some(tool)) = (regions.bottom_panel, self.layout.bottom) {
            root = root.child(self.tool_window(tool, rect, cx));
        }
        if let Some(index) = self.open_menu {
            root = root.child(self.menu_popup(index, &regions, cx));
        }
        if let Some(popup) = self.quick_popup(&regions, cx) {
            root = root.child(popup);
        }
        root
    }
}

impl Workbench {
    fn menu_bar(&self, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let items = self.menus.iter().enumerate().map(|(i, menu)| {
            let open = self.open_menu == Some(i);
            div()
                .id(("menu", i))
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .when(open, |d| d.bg(rgba(p.selection)))
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_menu(i, cx);
                }))
                .child(menu.title.clone())
        });
        place(rect)
            .bg(rgba(p.menu_bar))
            .flex()
            .items_center()
            .px_1()
            .children(items)
            .into_any_element()
    }

    /// The toolbar with the navigation bar of Classic UI: `project › dir › file`.
    fn toolbar(&self, rect: Rect) -> AnyElement {
        let p = &self.palette;
        let parts = self.navigation();
        let last = parts.len().saturating_sub(1);
        let crumbs = parts.into_iter().enumerate().flat_map(|(i, part)| {
            let crumb = div()
                .px_1()
                .child(part)
                .when(i == last, |d| d.text_color(rgba(p.foreground)));
            let separator = (i < last).then(|| div().text_color(rgba(p.muted)).child("›"));
            std::iter::once(crumb).chain(separator)
        });
        place(rect)
            .bg(rgba(p.toolbar))
            .border_b_1()
            .border_color(rgba(p.border))
            .flex()
            .items_center()
            .px_2()
            .text_color(rgba(p.muted))
            .children(crumbs)
            .into_any_element()
    }

    fn status_bar(&self, rect: Rect, cx: &App) -> AnyElement {
        let p = &self.palette;
        let format = self
            .split
            .tabs()
            .active()
            .and_then(|t| self.files.get(&t.uri))
            .map(|f| f.format.clone());
        let status = self.active_editor().map(|e| (e.status)(cx));
        // As IntelliJ: `line:column`, the number of carets when several, line ending, encoding.
        let right = match (&status, format) {
            (Some(s), Some(format)) => {
                let carets = if s.carets > 1 {
                    format!("{} carets   ", s.carets)
                } else {
                    String::new()
                };
                format!("{carets}{}:{}   {format}", s.line, s.column)
            }
            _ => String::new(),
        };
        let message = status
            .and_then(|s| s.message)
            .or_else(|| self.message.clone())
            .unwrap_or_default();
        place(rect)
            .bg(rgba(p.status_bar))
            .text_color(rgba(p.status_foreground))
            .border_t_1()
            .border_color(rgba(p.border))
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .text_size(px(12.0))
            .child(message)
            .child(right)
            .into_any_element()
    }

    fn menu_popup(&self, index: usize, regions: &Regions, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        // Menu titles are laid out by text width; the popup goes under its title at an
        // estimate, which is close enough for a list that opens downwards.
        let x = 4.0
            + self.menus[..index]
                .iter()
                .map(|m| m.title.chars().count() as f32 * 7.0 + 16.0)
                .sum::<f32>();
        let menu = &self.menus[index];
        let mut list = div()
            .absolute()
            .left(px(x))
            .top(px(regions.menu_bar.h))
            .min_w(px(260.0))
            .py_1()
            .bg(rgba(p.popup))
            .border_1()
            .border_color(rgba(p.border))
            .shadow_md()
            .flex()
            .flex_col();
        if menu.items.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(rgba(p.muted))
                    .child(self.t("menu.empty")),
            );
        }
        for (i, item) in menu.items.iter().enumerate() {
            let Some(item) = item else {
                list = list.child(div().my_1().h(px(1.0)).bg(rgba(p.border)));
                continue;
            };
            let command = item.command.clone();
            let enabled = item.enabled;
            list = list.child(
                div()
                    .id(("menu-item", i))
                    .px_3()
                    .py_1()
                    .flex()
                    .justify_between()
                    .gap_6()
                    .when(!enabled, |d| d.text_color(rgba(p.muted)))
                    .when(enabled, |d| {
                        d.hover(|d| d.bg(rgba(p.selection))).on_click(cx.listener(
                            move |this, _: &ClickEvent, _window, cx| {
                                this.execute(command.clone(), Vec::new(), cx);
                            },
                        ))
                    })
                    .child(item.title.clone())
                    .child(
                        div()
                            .text_color(rgba(p.muted))
                            .child(item.keys.clone().unwrap_or_default()),
                    ),
            );
        }
        // A click anywhere else closes the menu.
        div()
            .id("menu-overlay")
            .absolute()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.open_menu = None;
                    cx.notify();
                }),
            )
            .child(list.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }

    fn problems_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        if self.problems.is_empty() {
            return div()
                .p_2()
                .text_color(rgba(p.muted))
                .child(self.t("problems.none"))
                .into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        for (g, group) in self.problems.iter().enumerate() {
            let open = group.location.clone();
            rows.push(
                div()
                    .id(("problem-group", g))
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .whitespace_nowrap()
                    .hover(|d| d.bg(rgba(p.hover)))
                    .when_some(open, |d, uri| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.execute_id(
                                "workbench.openFile",
                                vec![Value::from(uri.as_str())],
                                cx,
                            );
                        }))
                    })
                    .child(group.source.clone())
                    .child(
                        div()
                            .text_color(rgba(p.muted))
                            .child(group.problems.len().to_string()),
                    )
                    .into_any_element(),
            );
            for (i, problem) in group.problems.iter().enumerate() {
                let color = match problem.severity {
                    Severity::Error => p.error,
                    Severity::Warning => p.warning,
                    Severity::Info => p.info,
                };
                rows.push(
                    div()
                        .id(("problem", g * 10_000 + i))
                        .min_h(px(ROW_HEIGHT))
                        .pl(px(26.0))
                        .pr_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(rgba(color)).child("●"))
                        .child(problem.message.clone())
                        .into_any_element(),
                );
            }
        }
        div()
            .id("problems")
            .flex_1()
            .overflow_y_scroll()
            .py_1()
            .children(rows)
            .into_any_element()
    }

    fn output_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let channels = self.core.output().channels();
        let selector = channels.into_iter().map(|channel| {
            let selected = self.output_channel.as_deref() == Some(channel.as_str());
            let title = self.channel_title(&channel);
            div()
                .id(SharedString::from(format!("channel-{channel}")))
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .when(selected, |d| d.bg(rgba(p.stripe_active)))
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.output_channel = Some(channel.clone());
                    this.output_lines = Arc::new(Vec::new());
                    this.refresh_output();
                    cx.notify();
                }))
                .child(title)
        });
        let clear = self.output_channel.clone().map(|channel| {
            div()
                .id("clear-output")
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .text_color(rgba(p.muted))
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.core.output().clear(&channel);
                    this.refresh_output();
                    cx.notify();
                }))
                .child(self.t("output.clear"))
        });
        let bar = div()
            .h(px(24.0))
            .flex_none()
            .flex()
            .justify_between()
            .border_b_1()
            .border_color(rgba(p.border))
            .child(div().flex().h_full().children(selector))
            .children(clear);
        let lines = self.output_lines.clone();
        let list = uniform_list(
            "output-lines",
            lines.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                range
                    .map(|i| {
                        div()
                            .id(i)
                            .h(px(17.0))
                            .px_2()
                            .whitespace_nowrap()
                            .child(lines[i].clone())
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.output_scroll.clone())
        .flex_1()
        .w_full()
        .font_family(MONOSPACE)
        .text_size(px(12.0));
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(bar)
            .child(list)
            .into_any_element()
    }

    /// The Quick Open popup: an input and up to [`QUICK_ROWS`] results around the selection.
    fn quick_popup(&self, regions: &Regions, cx: &mut Context<Self>) -> Option<AnyElement> {
        let quick = self.quick_open.as_ref()?;
        let p = &self.palette;
        let width = (regions.menu_bar.w * 0.5).clamp(420.0, 720.0);
        let x = ((regions.menu_bar.w - width) / 2.0).max(0.0);
        let (before, after) = quick.input.split();
        let caret = div().w(px(1.0)).h(px(16.0)).bg(rgba(p.foreground));
        let input = div()
            .h(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .bg(rgba(p.input))
            .border_b_1()
            .border_color(rgba(p.border))
            .whitespace_nowrap()
            .child(before.to_owned())
            .child(caret)
            .child(after.to_owned())
            .when(quick.input.text().is_empty(), |d| {
                d.child(
                    div()
                        .pl_2()
                        .text_color(rgba(p.muted))
                        .child(self.t("quickOpen.placeholder")),
                )
            });
        let first = quick.selected.saturating_sub(QUICK_ROWS - 1);
        let rows = quick
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(QUICK_ROWS)
            .map(|(i, item)| {
                let selected = i == quick.selected;
                // Matches stand out in the accent colour, on the selection in the text colour.
                let emphasis = HighlightStyle {
                    color: Some(rgba(if selected { p.foreground } else { p.accent }).into()),
                    font_weight: Some(FontWeight::BOLD),
                    ..HighlightStyle::default()
                };
                let label = StyledText::new(item.label.clone()).with_highlights(char_ranges(
                    &item.label,
                    &item.highlights,
                    emphasis,
                ));
                div()
                    .id(("quick-item", i))
                    .h(px(24.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when(selected, |d| d.bg(rgba(p.selection)))
                    .when(item.action == Action::None, |d| d.text_color(rgba(p.muted)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.choose(i, cx);
                    }))
                    .child(label)
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_color(rgba(p.muted))
                            .text_size(px(12.0))
                            .child(item.detail.clone()),
                    )
                    .when_some(item.keys.clone(), |d, keys| {
                        d.child(div().text_color(rgba(p.muted)).child(keys))
                    })
            });
        let popup = div()
            .id("quick-open")
            .track_focus(&self.quick_focus)
            .on_key_down(cx.listener(Self::quick_key))
            .absolute()
            .left(px(x))
            .top(px(regions.toolbar.y + regions.toolbar.h + 40.0))
            .w(px(width))
            .bg(rgba(p.popup))
            .border_1()
            .border_color(rgba(p.focus_border))
            .shadow_lg()
            .flex()
            .flex_col()
            .child(input)
            .child(div().py_1().flex().flex_col().children(rows))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // A click outside closes the popup.
        Some(
            div()
                .id("quick-overlay")
                .absolute()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.close_quick();
                        cx.notify();
                    }),
                )
                .child(popup)
                .into_any_element(),
        )
    }

    /// A stripe with the buttons of the tool windows on its edge.
    fn stripe(&self, side: Side, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let buttons = ToolWindow::ALL
            .into_iter()
            .filter(|w| w.side() == side)
            .map(|tool| {
                let open = self.layout.is_open(tool);
                let mut title = self.t(tool.title_key());
                // The Problems button tells how many there are.
                let (errors, warnings) = problems::counts(&self.problems);
                let count = errors + warnings;
                if tool == ToolWindow::Problems && count > 0 {
                    title = format!("{title} {count}");
                }
                let command = tool.toggle_command();
                let button = div()
                    .id(command)
                    .when(open, |d| d.bg(rgba(p.stripe_active)))
                    .hover(|d| d.bg(rgba(p.hover)))
                    .text_size(px(12.0))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.execute_id(command, Vec::new(), cx);
                    }));
                match side {
                    // Classic UI writes side buttons vertically; GPUI does not rotate text,
                    // so the letters are stacked.
                    Side::Left => button
                        .w_full()
                        .py_2()
                        .flex()
                        .flex_col()
                        .items_center()
                        .children(
                            title
                                .chars()
                                .map(|c| div().h(px(13.0)).child(c.to_string())),
                        ),
                    Side::Bottom => button.h_full().px_2().flex().items_center().child(title),
                }
            });
        let stripe = place(rect).bg(rgba(p.stripe)).border_color(rgba(p.border));
        match side {
            Side::Left => stripe.border_r_1().flex().flex_col().children(buttons),
            Side::Bottom => stripe.border_t_1().flex().children(buttons),
        }
        .into_any_element()
    }

    fn tool_window(&self, tool: ToolWindow, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let command = tool.toggle_command();
        let header = div()
            .h(px(24.0))
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgba(p.panel_header))
            .border_b_1()
            .border_color(rgba(p.border))
            .child(self.t(tool.title_key()))
            .child(
                div()
                    .id("hide")
                    .px_1()
                    .text_color(rgba(p.muted))
                    .hover(|d| d.bg(rgba(p.hover)))
                    .child("—")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.execute_id(command, Vec::new(), cx);
                    })),
            );
        let body = match tool {
            ToolWindow::Project => self.project_tree(cx),
            ToolWindow::Problems => self.problems_view(cx),
            ToolWindow::Output => self.output_view(cx),
        };
        let panel = place(rect)
            .bg(rgba(p.panel))
            .flex()
            .flex_col()
            .child(header)
            .child(body);
        match tool.side() {
            Side::Left => panel.border_r_1(),
            Side::Bottom => panel.border_t_1(),
        }
        .border_color(rgba(p.border))
        .into_any_element()
    }

    fn project_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        // The panel takes focus even before its first directory is listed.
        let panel = div()
            .id("project")
            .track_focus(&self.project_focus)
            .on_key_down(cx.listener(Self::tree_key))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden();
        if self.rows.is_empty() {
            return panel
                .child(
                    div()
                        .p_2()
                        .text_color(rgba(p.muted))
                        .child(self.t("project.noFolder")),
                )
                .into_any_element();
        }
        let list = uniform_list(
            "project-tree",
            self.rows.len(),
            cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                let p = this.palette.clone();
                let focused = this.project_focus.is_focused(window);
                let selected = this.tree.selected().cloned();
                range
                    .filter_map(|i| {
                        let row = this.rows.get(i)?.clone();
                        let is_selected = Some(&row.uri) == selected.as_ref();
                        let arrow = match (row.dir, row.expanded) {
                            (false, _) => " ",
                            (true, false) => "▸",
                            (true, true) => "▾",
                        };
                        let uri = row.uri.clone();
                        let selection = if focused {
                            p.selection
                        } else {
                            p.selection_inactive
                        };
                        Some(
                            div()
                                .id(i)
                                .h(px(ROW_HEIGHT))
                                .pl(px(6.0 + row.depth as f32 * 16.0))
                                .flex()
                                .items_center()
                                .gap_1()
                                .whitespace_nowrap()
                                .when(is_selected, |d| d.bg(rgba(selection)))
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        this.tree.select(uri.clone());
                                        this.project_focus.focus(window);
                                        if event.click_count() >= 2 {
                                            this.activate_selected(cx);
                                        }
                                        cx.notify();
                                    },
                                ))
                                .child(div().w(px(10.0)).text_color(rgba(p.muted)).child(arrow))
                                .child(row.name),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.tree_scroll.clone())
        .flex_1()
        .w_full();
        panel.child(list).into_any_element()
    }

    fn editor_area(&self, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        // The area's own colour shows as the divider between groups.
        let area = place(rect)
            .id("editor-area")
            .track_focus(&self.editor_focus)
            .bg(rgba(p.border));
        let groups = self.split.groups();
        if groups.len() == 1 && groups[0].len() == 0 {
            return area
                .bg(rgba(p.editor))
                .child(self.empty_editor())
                .into_any_element();
        }
        let rects = self.split.rects(Rect::new(0.0, 0.0, rect.w, rect.h));
        let children: Vec<AnyElement> = groups
            .iter()
            .zip(rects)
            .enumerate()
            .map(|(g, (tabs, r))| self.editor_group(g, tabs, r, cx))
            .collect();
        area.children(children).into_any_element()
    }

    /// One editor group: its tab bar and the active file. The active group's tab is
    /// underlined in the accent colour, the other group's in the border colour.
    fn editor_group(
        &self,
        g: usize,
        tabs: &Tabs,
        rect: Rect,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &self.palette;
        let focused_group = g == self.split.active_group();
        let active_index = tabs.active_index();
        let tab_items = tabs.iter().enumerate().map(|(i, tab)| {
            let is_active = Some(i) == active_index;
            let underline = if focused_group {
                p.tab_underline
            } else {
                p.border
            };
            div()
                .id(("tab", g * 10_000 + i))
                .h_full()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_r_1()
                .border_color(rgba(p.border))
                .bg(rgba(if is_active {
                    p.tab_active
                } else {
                    p.tab_inactive
                }))
                .when(is_active, |d| d.border_b_2().border_color(rgba(underline)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.split.focus_group(g);
                    this.split.tabs_mut().activate(i);
                    this.focus_request = Some(FocusTarget::Editor);
                    this.publish_context();
                    this.save_state();
                    cx.notify();
                }))
                .child(tab.title.clone())
                .child(
                    div()
                        .id(("close", g * 10_000 + i))
                        .text_color(rgba(p.muted))
                        .hover(|d| d.text_color(rgba(p.foreground)))
                        .child("×")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.split.focus_group(g);
                            this.split.tabs_mut().activate(i);
                            this.execute_id("workbench.closeEditor", Vec::new(), cx);
                        })),
                )
        });
        let tab_bar = div()
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .flex()
            .bg(rgba(p.tab_inactive))
            .border_b_1()
            .border_color(rgba(p.border))
            .children(tab_items);
        let body = tabs
            .active()
            .and_then(|tab| self.editors.get(&(g, tab.uri.clone())))
            .map_or_else(
                || div().flex_1().into_any_element(),
                |editor| {
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(editor.view.clone())
                        .into_any_element()
                },
            );
        place(rect)
            .id(("editor-group", g))
            .bg(rgba(p.editor))
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _window, cx| {
                    if this.split.active_group() != g {
                        this.split.focus_group(g);
                        this.publish_context();
                        this.save_state();
                        cx.notify();
                    }
                }),
            )
            .child(tab_bar)
            .child(body)
            .into_any_element()
    }

    fn empty_editor(&self) -> AnyElement {
        let p = &self.palette;
        let lookup = self.lookup();
        let hints: Vec<(String, String)> = [
            "workbench.quickOpen",
            "workbench.showCommands",
            "workbench.toggleProject",
        ]
        .into_iter()
        .filter_map(|id| CommandId::new(id).ok())
        .filter_map(|id| Some((lookup.title(&id)?, lookup.keys(&id)?)))
        .collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(rgba(p.muted))
            .child(div().text_size(px(15.0)).child(self.t("editor.empty")))
            .children(hints.into_iter().map(|(title, keys)| {
                div()
                    .flex()
                    .gap_3()
                    .child(title)
                    .child(div().text_color(rgba(p.accent)).child(keys))
            }))
            .into_any_element()
    }
}

/// Byte ranges of the given character positions, merged where adjacent, with one style.
fn char_ranges(
    text: &str,
    chars: &[u32],
    style: HighlightStyle,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut ranges: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for (index, (start, c)) in text.char_indices().enumerate() {
        if !chars.contains(&(index as u32)) {
            continue;
        }
        let end = start + c.len_utf8();
        match ranges.last_mut() {
            Some((range, _)) if range.end == start => range.end = end,
            _ => ranges.push((start..end, style)),
        }
    }
    ranges
}
