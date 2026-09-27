//! The workbench window (spec, section 20) in the Classic UI arrangement of JetBrains IDEs:
//! menu bar, toolbar with the navigation bar, tool-window stripes, tool windows, editor tabs,
//! status bar. Geometry comes from [`LayoutState::regions`], the project tree and the tabs are
//! [`FileTree`] and [`Tabs`]; this module draws them and routes input to commands.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, HighlightStyle, KeyDownEvent,
    MouseButton, ScrollStrategy, SharedString, StyledText, UniformListScrollHandle, Window, div,
    prelude::*, px, rgba, uniform_list,
};
use kumiko_core::CoreApi;
use kumiko_core::config::keymap::{KeyChord, KeySequence, Resolution};
use kumiko_core::documents::{Encoding, LineEnding, Snapshot};
use kumiko_core::events::{Delivery, EventFilter};
use kumiko_core::protocol::events::FsChanged;
use kumiko_core::protocol::methods::fs::FileKind;
use kumiko_core::service_api::FileSystem;
use kumiko_types::{CommandId, DocumentId, Uri};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::a11y::{AccessNode, AccessTree, Bridge};
use crate::commands::UiCommand;
use crate::keys;
use crate::layout::{LayoutState, Rect, Regions, Side, TAB_BAR_HEIGHT, ToolWindow};
use crate::menus::{self, CommandLookup, Menu};
use crate::palette::Palette;
use crate::quick_open::{self, Action, Item, Mode, QuickOpen};
use crate::tabs::Tabs;
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
const EDITOR_TEXT: f32 = 14.0;
const EDITOR_LINE: f32 = 19.0;
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

/// A file shown in a tab: its document and its lines, as text for display.
struct OpenFile {
    doc: DocumentId,
    lines: Arc<Vec<SharedString>>,
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
    tabs: Tabs,
    files: HashMap<Uri, OpenFile>,
    editor_scroll: UniformListScrollHandle,
    /// The last message for the status bar (e.g. a failed command).
    message: Option<String>,
    a11y: Option<Bridge>,
    /// The open Quick Open popup and its focus.
    quick_open: Option<QuickOpen>,
    quick_focus: FocusHandle,
    /// Files opened, most recent first (shown first in Quick Open).
    recent: Vec<Uri>,
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
            tabs: Tabs::default(),
            files: HashMap::new(),
            editor_scroll: UniformListScrollHandle::new(),
            message: None,
            a11y: None,
            started: Some(started),
            shown: false,
            quick_open: None,
            quick_focus: cx.focus_handle(),
            recent: Vec::new(),
        };
        workbench.menus = workbench.build_menus();
        workbench.watch_events(cx);
        workbench.refresh_roots(cx);
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
            UiCommand::CloseEditor => self.close_active_tab(),
            UiCommand::NextEditor => self.tabs.cycle(1),
            UiCommand::PreviousEditor => self.tabs.cycle(-1),
            UiCommand::Quit => cx.quit(),
        }
        self.publish_context();
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
        context.set("activeEditorCount", Value::from(self.tabs.len()));
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

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
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
        match keymap.resolve(&pressed, self.core.commands().context()) {
            Resolution::Command(id) => {
                self.pending.clear();
                cx.stop_propagation();
                self.execute(id, Vec::new(), cx);
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
                self.refresh_roots(cx);
                let dirs: Vec<Uri> = self.tree.loaded_dirs().cloned().collect();
                for dir in dirs {
                    self.load_dir(dir, cx);
                }
                return;
            }
        };
        if let Some(Ok(changed)) = event.decode::<FsChanged>() {
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
            self.tabs.open(uri, title);
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
                        workbench.tabs.open(uri, title);
                        workbench
                            .editor_scroll
                            .scroll_to_item(0, ScrollStrategy::Top);
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

    fn close_active_tab(&mut self) {
        let Some(index) = self.tabs.active_index() else {
            return;
        };
        if let Some(tab) = self.tabs.close(index)
            && let Some(file) = self.files.remove(&tab.uri)
            && let Err(error) = self.core.documents().close(file.doc)
        {
            tracing::warn!("closing {}: {error}", tab.uri);
        }
        if self.tabs.len() == 0 {
            self.focus_request = Some(FocusTarget::Project);
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
        self.focus_request = Some(if self.tabs.len() > 0 {
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
        let Some(file) = self.tabs.active().and_then(|t| self.files.get(&t.uri)) else {
            return vec![self.note("quickOpen.noFile")];
        };
        match quick_open::parse_line(query) {
            Some(line) => {
                let line = line.min(file.lines.len().max(1));
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
            Action::GotoLine(line) => self
                .editor_scroll
                .scroll_to_item(line.saturating_sub(1), ScrollStrategy::Center),
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
        let tab_ids: Vec<u64> = (0..self.tabs.len() as u64)
            .map(|i| node::TABS + i)
            .collect();
        let editor_label = self
            .tabs
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
        for (i, tab) in self.tabs.iter().enumerate() {
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
        let Some(tab) = self.tabs.active() else {
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
            lines: Arc::new(lines),
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
                _ => self.editor_focus.focus(window),
            }
        }
        let focus = (
            self.project_focus.is_focused(window),
            self.editor_focus.is_focused(window),
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
            .child(self.status_bar(regions.status_bar));
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

    fn status_bar(&self, rect: Rect) -> AnyElement {
        let p = &self.palette;
        let right = self
            .tabs
            .active()
            .and_then(|t| self.files.get(&t.uri))
            .map(|f| format!("{}   {}", f.lines.len(), f.format))
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
            .child(self.message.clone().unwrap_or_default())
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
                let title = self.t(tool.title_key());
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
            ToolWindow::Problems | ToolWindow::Output => div()
                .p_2()
                .text_color(rgba(p.muted))
                .child(self.t("panel.empty"))
                .into_any_element(),
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
        let area = place(rect)
            .id("editor-area")
            .track_focus(&self.editor_focus)
            .bg(rgba(p.editor))
            .flex()
            .flex_col();
        let Some(active) = self.tabs.active() else {
            return area.child(self.empty_editor()).into_any_element();
        };
        let active_index = self.tabs.active_index();
        let tabs = self.tabs.iter().enumerate().map(|(i, tab)| {
            let is_active = Some(i) == active_index;
            div()
                .id(("tab", i))
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
                .when(is_active, |d| {
                    d.border_b_2().border_color(rgba(p.tab_underline))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tabs.activate(i);
                    this.focus_request = Some(FocusTarget::Editor);
                    cx.notify();
                }))
                .child(tab.title.clone())
                .child(
                    div()
                        .id(("close", i))
                        .text_color(rgba(p.muted))
                        .hover(|d| d.text_color(rgba(p.foreground)))
                        .child("×")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.tabs.activate(i);
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
            .children(tabs);
        let body = match self.files.get(&active.uri) {
            Some(file) => self.file_view(file, cx),
            None => div().into_any_element(),
        };
        area.child(tab_bar).child(body).into_any_element()
    }

    /// A read-only view of the file's lines; editing comes with the editor view (E8).
    fn file_view(&self, file: &OpenFile, cx: &mut Context<Self>) -> AnyElement {
        let lines = file.lines.clone();
        let digits = lines.len().max(1).to_string().len();
        let gutter = 16.0 + digits as f32 * 8.5;
        uniform_list(
            "file-view",
            lines.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                let p = &this.palette;
                range
                    .map(|i| {
                        div()
                            .id(i)
                            .h(px(EDITOR_LINE))
                            .flex()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .w(px(gutter))
                                    .flex_none()
                                    .pr_2()
                                    .flex()
                                    .justify_end()
                                    .bg(rgba(p.panel))
                                    .text_color(rgba(p.muted))
                                    .child(format!("{}", i + 1)),
                            )
                            .child(div().pl_2().child(lines[i].clone()))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.editor_scroll.clone())
        .flex_1()
        .w_full()
        .font_family(MONOSPACE)
        .text_size(px(EDITOR_TEXT))
        .text_color(rgba(self.palette.editor_foreground))
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
