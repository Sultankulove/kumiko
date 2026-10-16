//! The editor view on GPUI: draws a document through the display map and turns keys, text
//! input (with IME), mouse and wheel into the model's moves and edits. Edits go to the
//! Document Service as transactions; changes made elsewhere arrive as `document.changed`
//! and move the carets.
//!
//! Text input follows GPUI's own `examples/input.rs` (`EntityInputHandler`). Text being
//! composed by an IME is put into the document and underlined until it is committed, as Zed
//! does; it joins the surrounding undo group.

use std::ops::Range as StdRange;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, Hsla, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, div, fill, font, point, prelude::*, px, relative, size,
};
use kumiko_core::CoreApi;
use kumiko_core::events::{Delivery, EventFilter};
use kumiko_core::protocol::events::DocumentChanged;
use kumiko_types::{DocumentId, DocumentVersion, Uri};
use ropey::Rope;
use tokio::runtime::Handle;

use crate::carets;
use crate::display::{DisplayConfig, DisplayMap};
use crate::editing::{self, Copied, Edit, Indent};
use crate::movement::{Motion, move_selection};
use crate::selection::{Assoc, Change, Range, Selection, map_position};
use crate::text::{char_of, char_of_utf16, utf16_of};

/// Colours and font of the editor, from the theme and settings.
#[derive(Debug, Clone)]
pub struct EditorStyle {
    pub background: Hsla,
    pub foreground: Hsla,
    pub gutter_background: Hsla,
    pub gutter_foreground: Hsla,
    pub active_line_number: Hsla,
    pub line_highlight: Hsla,
    pub selection: Hsla,
    pub caret: Hsla,
    /// Capture name, colour, bold, italic.
    pub syntax: Vec<(String, Hsla, bool, bool)>,
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
}

/// Commands the editor runs itself (registered in the Command Service by the workbench, and
/// run directly when their keys are pressed in an editor).
pub const COMMANDS: &[&str] = &[
    "editor.cursorLeft",
    "editor.cursorRight",
    "editor.cursorUp",
    "editor.cursorDown",
    "editor.cursorWordLeft",
    "editor.cursorWordRight",
    "editor.cursorLineStart",
    "editor.cursorLineEnd",
    "editor.cursorPageUp",
    "editor.cursorPageDown",
    "editor.cursorTop",
    "editor.cursorBottom",
    "editor.selectLeft",
    "editor.selectRight",
    "editor.selectUp",
    "editor.selectDown",
    "editor.selectWordLeft",
    "editor.selectWordRight",
    "editor.selectLineStart",
    "editor.selectLineEnd",
    "editor.selectPageUp",
    "editor.selectPageDown",
    "editor.selectTop",
    "editor.selectBottom",
    "editor.selectAll",
    "editor.deleteLeft",
    "editor.deleteRight",
    "editor.deleteWordLeft",
    "editor.deleteWordRight",
    "editor.newline",
    "editor.indent",
    "editor.outdent",
    "editor.duplicate",
    "editor.deleteLine",
    "editor.moveLineUp",
    "editor.moveLineDown",
    "editor.copy",
    "editor.cut",
    "editor.paste",
    "editor.cloneCaretAbove",
    "editor.cloneCaretBelow",
    "editor.selectNextOccurrence",
    "editor.unselectOccurrence",
    "editor.selectAllOccurrences",
    "editor.escape",
    "editor.toggleSoftWrap",
    "edit.undo",
    "edit.redo",
    "file.save",
];

/// What was drawn last, for hit testing and the IME candidate window.
struct Layout {
    bounds: Bounds<Pixels>,
    /// Left edge of the text (after the gutter), in window coordinates.
    text_left: Pixels,
    first_row: usize,
    rows: Vec<RowLayout>,
}

struct RowLayout {
    start: usize,
    /// Byte offset in the shaped text of each char of the row, plus one past the end.
    offsets: Vec<usize>,
    line: ShapedLine,
}

impl RowLayout {
    fn x_for(&self, char: usize) -> Pixels {
        let index = char.saturating_sub(self.start).min(self.offsets.len() - 1);
        self.line.x_for_index(self.offsets[index])
    }

    fn char_for(&self, x: Pixels) -> usize {
        let byte = self.line.closest_index_for_x(x);
        let index = self
            .offsets
            .partition_point(|&o| o <= byte)
            .saturating_sub(1);
        self.start + index
    }
}

pub struct EditorView {
    core: CoreApi,
    runtime: Handle,
    doc: DocumentId,
    uri: Uri,
    rope: Rope,
    version: DocumentVersion,
    read_only: bool,
    selection: Selection,
    map: DisplayMap,
    indent: Indent,
    style: EditorStyle,
    focus: FocusHandle,
    scroll: Point<Pixels>,
    /// IME composition: the marked chars in the document.
    marked: Option<StdRange<usize>>,
    /// Undo/redo: carets go to the changes of the next document change.
    carets_from_next_change: bool,
    /// The last copy, to paste one part per caret.
    copied: Option<Copied>,
    /// A mouse drag extends the primary range.
    dragging: bool,
    layout: Option<Layout>,
    /// Text for the status bar after a failed action (e.g. a read-only file).
    pub message: Option<String>,
}

impl EditorView {
    pub fn new(
        core: CoreApi,
        runtime: Handle,
        snapshot: &kumiko_core::documents::Snapshot,
        style: EditorStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let tab_width = core
            .config()
            .get("editor.tabSize", None)
            .and_then(|v| v.as_u64())
            .unwrap_or(4) as usize;
        let config = DisplayConfig {
            tab_width,
            wrap: None,
        };
        let map = DisplayMap::new(&snapshot.text, config);
        let view = Self {
            core,
            runtime,
            doc: snapshot.id,
            uri: snapshot.uri.clone(),
            rope: snapshot.text.clone(),
            version: snapshot.version,
            read_only: snapshot.read_only,
            selection: Selection::point(0),
            map,
            indent: Indent {
                width: tab_width,
                tabs: false,
            },
            style,
            focus: cx.focus_handle(),
            scroll: Point::default(),
            marked: None,
            carets_from_next_change: false,
            copied: None,
            dragging: false,
            layout: None,
            message: None,
        };
        view.follow_changes(cx);
        view
    }

    pub fn document(&self) -> DocumentId {
        self.doc
    }

    pub fn set_style(&mut self, style: EditorStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// `line:column` of the primary caret, 1-based, for the status bar.
    pub fn caret_position(&self) -> (usize, usize) {
        let head = self.selection.primary().head;
        let line = self.rope.char_to_line(head);
        (line + 1, head - self.rope.line_to_char(line) + 1)
    }

    pub fn caret_count(&self) -> usize {
        self.selection.len()
    }

    /// Puts the caret at the start of a line (1-based) and scrolls to it.
    pub fn goto_line(&mut self, line: usize, cx: &mut Context<Self>) {
        let line = line
            .saturating_sub(1)
            .min(self.rope.len_lines().saturating_sub(1));
        self.selection = Selection::point(self.rope.line_to_char(line));
        self.reveal();
        if let Some(layout) = &self.layout {
            // Centre the line, as IntelliJ does after Go to Line.
            let half = f32::from(layout.bounds.size.height) / 2.0;
            let (row, _) = self.map.locate(&self.rope, self.selection.primary().head);
            self.scroll.y = px((row as f32 * self.style.line_height - half).max(0.0));
        }
        cx.notify();
    }

    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Changes made elsewhere (another view, a plugin, a reload, undo) move the carets.
    fn follow_changes(&self, cx: &mut Context<Self>) {
        let mut events = self
            .core
            .events()
            .subscribe(EventFilter::topics(["document.changed"]));
        let uri = self.uri.clone();
        cx.spawn(async move |this, cx| {
            while let Some(delivery) = events.recv().await {
                let changed = match delivery {
                    Delivery::Event(event) => match event.decode::<DocumentChanged>() {
                        Some(Ok(changed)) if changed.uri == uri => Some(changed),
                        _ => continue,
                    },
                    Delivery::Resync(_) => None,
                };
                if this
                    .update(cx, |view, cx| view.on_changed(changed, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_changed(&mut self, changed: Option<DocumentChanged>, cx: &mut Context<Self>) {
        let Ok(snapshot) = self.core.documents().snapshot(self.doc) else {
            return;
        };
        if snapshot.version <= self.version {
            return;
        }
        // The event's edits refer to the text before them; only one step is mapped, a gap
        // (lost events) keeps the carets clamped instead.
        let changes: Option<Vec<Change>> = changed
            .filter(|c| c.version.0 == self.version.0 + 1)
            .map(|c| {
                let mut list: Vec<Change> = c
                    .changes
                    .iter()
                    .map(|e| Change {
                        start: char_of(&self.rope, e.range.start),
                        end: char_of(&self.rope, e.range.end),
                        text: e.new_text.clone(),
                    })
                    .collect();
                list.sort_by_key(|c| c.start);
                list
            });
        let selection = match (&changes, self.carets_from_next_change) {
            (Some(changes), true) if !changes.is_empty() => {
                let carets: Vec<Range> = changes
                    .iter()
                    .map(|c| {
                        Range::point(
                            map_position(c.start, changes, Assoc::Before) + c.text.chars().count(),
                        )
                    })
                    .collect();
                Selection::new(carets, 0)
            }
            (Some(changes), _) => self.selection.map(changes),
            (None, _) => self.selection.clamp(snapshot.text.len_chars()),
        };
        self.carets_from_next_change = false;
        self.load(snapshot);
        self.selection = selection.clamp(self.rope.len_chars());
        self.reveal();
        cx.notify();
    }

    fn load(&mut self, snapshot: kumiko_core::documents::Snapshot) {
        self.rope = snapshot.text;
        self.version = snapshot.version;
        self.read_only = snapshot.read_only;
        self.map.rebuild(&self.rope);
    }

    /// Sends a transaction to the Document Service and shows the result at once.
    fn commit(&mut self, edit: Edit, cx: &mut Context<Self>) {
        if edit.is_empty() {
            self.selection = edit.selection;
            self.reveal();
            cx.notify();
            return;
        }
        let edits = edit.text_edits(&self.rope);
        match self.core.documents().apply(self.doc, self.version, edits) {
            Ok(_) => match self.core.documents().snapshot(self.doc) {
                Ok(snapshot) => {
                    self.load(snapshot);
                    self.selection = edit.selection.clamp(self.rope.len_chars());
                    self.message = None;
                }
                Err(error) => self.message = Some(error.to_string()),
            },
            Err(error) => self.message = Some(error.to_string()),
        }
        self.reveal();
        cx.notify();
    }

    fn rows_per_page(&self) -> usize {
        self.layout
            .as_ref()
            .map_or(30, |l| {
                (f32::from(l.bounds.size.height) / self.style.line_height) as usize
            })
            .max(1)
    }

    /// Scrolls so the primary caret is visible.
    fn reveal(&mut self) {
        let Some(layout) = &self.layout else {
            return;
        };
        let line_height = self.style.line_height;
        let (row, column) = self.map.locate(&self.rope, self.selection.primary().head);
        let top = row as f32 * line_height;
        let height = f32::from(layout.bounds.size.height);
        let scroll_y = f32::from(self.scroll.y);
        if top < scroll_y {
            self.scroll.y = px(top);
        } else if top + line_height > scroll_y + height {
            self.scroll.y = px(top + line_height - height);
        }
        // Horizontally by columns of the monospace font (a close estimate is enough).
        let char_width = self.style.font_size * 0.6;
        let x = column as f32 * char_width;
        let width = f32::from(layout.bounds.right() - layout.text_left) - char_width * 2.0;
        let scroll_x = f32::from(self.scroll.x);
        if x < scroll_x {
            self.scroll.x = px((x - char_width * 4.0).max(0.0));
        } else if x > scroll_x + width {
            self.scroll.x = px(x - width + char_width * 4.0);
        }
    }

    fn clamp_scroll(&mut self) {
        let max_y = (self.map.row_count().saturating_sub(1)) as f32 * self.style.line_height;
        self.scroll.y = px(f32::from(self.scroll.y).clamp(0.0, max_y));
        self.scroll.x = px(f32::from(self.scroll.x).max(0.0));
    }

    fn moved(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        self.selection = move_selection(&self.rope, &self.map, &self.selection, motion, extend);
        self.reveal();
        cx.notify();
    }

    /// Runs an editor command; `false` if the id is not one of [`COMMANDS`].
    pub fn run(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        let page = self.rows_per_page();
        let (motion, extend) = match command {
            "editor.cursorLeft" => (Some(Motion::Left), false),
            "editor.cursorRight" => (Some(Motion::Right), false),
            "editor.cursorUp" => (Some(Motion::Up), false),
            "editor.cursorDown" => (Some(Motion::Down), false),
            "editor.cursorWordLeft" => (Some(Motion::WordLeft), false),
            "editor.cursorWordRight" => (Some(Motion::WordRight), false),
            "editor.cursorLineStart" => (Some(Motion::LineStart), false),
            "editor.cursorLineEnd" => (Some(Motion::LineEnd), false),
            "editor.cursorPageUp" => (Some(Motion::PageUp(page)), false),
            "editor.cursorPageDown" => (Some(Motion::PageDown(page)), false),
            "editor.cursorTop" => (Some(Motion::DocumentStart), false),
            "editor.cursorBottom" => (Some(Motion::DocumentEnd), false),
            "editor.selectLeft" => (Some(Motion::Left), true),
            "editor.selectRight" => (Some(Motion::Right), true),
            "editor.selectUp" => (Some(Motion::Up), true),
            "editor.selectDown" => (Some(Motion::Down), true),
            "editor.selectWordLeft" => (Some(Motion::WordLeft), true),
            "editor.selectWordRight" => (Some(Motion::WordRight), true),
            "editor.selectLineStart" => (Some(Motion::LineStart), true),
            "editor.selectLineEnd" => (Some(Motion::LineEnd), true),
            "editor.selectPageUp" => (Some(Motion::PageUp(page)), true),
            "editor.selectPageDown" => (Some(Motion::PageDown(page)), true),
            "editor.selectTop" => (Some(Motion::DocumentStart), true),
            "editor.selectBottom" => (Some(Motion::DocumentEnd), true),
            _ => (None, false),
        };
        if let Some(motion) = motion {
            self.moved(motion, extend, cx);
            return true;
        }
        let rope = self.rope.clone();
        let edit = match command {
            "editor.deleteLeft" => editing::backspace(&rope, &self.selection),
            "editor.deleteRight" => editing::delete(&rope, &self.selection),
            "editor.deleteWordLeft" => editing::delete_word_left(&rope, &self.selection),
            "editor.deleteWordRight" => editing::delete_word_right(&rope, &self.selection),
            "editor.newline" => editing::newline(&rope, &self.selection, self.indent),
            "editor.indent" => editing::indent(&rope, &self.selection, self.indent),
            "editor.outdent" => editing::outdent(&rope, &self.selection, self.indent),
            "editor.duplicate" => editing::duplicate(&rope, &self.selection),
            "editor.deleteLine" => editing::delete_lines(&rope, &self.selection),
            "editor.moveLineUp" => editing::move_lines(&rope, &self.selection, true),
            "editor.moveLineDown" => editing::move_lines(&rope, &self.selection, false),
            "editor.cut" => {
                let (copied, edit) = editing::cut(&rope, &self.selection);
                cx.write_to_clipboard(ClipboardItem::new_string(copied.text.clone()));
                self.copied = Some(copied);
                edit
            }
            "editor.paste" => {
                let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
                    return true;
                };
                editing::paste(&rope, &self.selection, &text, self.copied.as_ref())
            }
            _ => return self.run_other(command, cx),
        };
        self.commit(edit, cx);
        true
    }

    fn run_other(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        let rope = self.rope.clone();
        match command {
            "editor.selectAll" => self.selection = carets::select_all(&rope),
            "editor.copy" => {
                let copied = editing::copy(&rope, &self.selection);
                cx.write_to_clipboard(ClipboardItem::new_string(copied.text.clone()));
                self.copied = Some(copied);
            }
            "editor.cloneCaretAbove" => {
                self.selection = carets::clone_caret(&rope, &self.map, &self.selection, true)
            }
            "editor.cloneCaretBelow" => {
                self.selection = carets::clone_caret(&rope, &self.map, &self.selection, false)
            }
            "editor.selectNextOccurrence" => {
                self.selection = carets::select_next_occurrence(&rope, &self.selection)
            }
            "editor.unselectOccurrence" => {
                self.selection = carets::unselect_occurrence(&self.selection)
            }
            "editor.selectAllOccurrences" => {
                self.selection = carets::select_all_occurrences(&rope, &self.selection)
            }
            "editor.escape" => {
                let primary = self.selection.primary();
                self.selection = if self.selection.len() > 1 {
                    self.selection.only_primary()
                } else {
                    Selection::point(primary.head)
                };
            }
            "editor.toggleSoftWrap" => {
                let mut config = self.map.config();
                config.wrap = match config.wrap {
                    Some(_) => None,
                    None => Some(self.wrap_columns()),
                };
                self.map.set_config(&rope, config);
                self.scroll.x = px(0.0);
            }
            "edit.undo" | "edit.redo" => {
                self.carets_from_next_change = true;
                let documents = self.core.documents();
                let done = if command == "edit.undo" {
                    documents.undo(self.doc)
                } else {
                    documents.redo(self.doc)
                };
                match done {
                    Ok(Some(_)) => {}
                    Ok(None) => self.carets_from_next_change = false,
                    Err(error) => {
                        self.carets_from_next_change = false;
                        self.message = Some(error.to_string());
                    }
                }
            }
            "file.save" => {
                let core = self.core.clone();
                let doc = self.doc;
                let task = self
                    .runtime
                    .spawn(async move { core.documents().save(doc).await });
                cx.spawn(async move |this, cx| {
                    if let Ok(Err(error)) = task.await {
                        let _ = this.update(cx, |view, cx| {
                            view.message = Some(error.to_string());
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            _ => return false,
        }
        self.reveal();
        cx.notify();
        true
    }

    /// Soft wrap fits the visible width in columns of the monospace font.
    fn wrap_columns(&self) -> usize {
        let width = self
            .layout
            .as_ref()
            .map_or(800.0, |l| f32::from(l.bounds.right() - l.text_left));
        ((width / (self.style.font_size * 0.6)) as usize)
            .saturating_sub(2)
            .max(20)
    }

    /// Re-wraps when the width changes (soft wrap on).
    fn rewrap(&mut self) {
        let mut config = self.map.config();
        if config.wrap.is_some() {
            config.wrap = Some(self.wrap_columns());
            self.map.set_config(&self.rope, config);
        }
    }

    // ---- mouse ----------------------------------------------------------------------------

    /// The char under a window point (clamped to the text).
    fn char_at_point(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let y = f32::from(position.y - layout.bounds.top()) + f32::from(self.scroll.y);
        let row = (y / self.style.line_height).floor().max(0.0) as usize;
        let row = row.min(self.map.row_count() - 1);
        let x = position.x - layout.text_left + self.scroll.x;
        match row
            .checked_sub(layout.first_row)
            .and_then(|i| layout.rows.get(i))
        {
            Some(row_layout) => Some(row_layout.char_for(x)),
            None => {
                let column = (f32::from(x) / (self.style.font_size * 0.6)).max(0.0) as usize;
                Some(self.map.char_at(&self.rope, row, column))
            }
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let Some(at) = self.char_at_point(event.position) else {
            return;
        };
        let modifiers = event.modifiers;
        self.selection = if modifiers.alt && modifiers.shift {
            // Alt+Shift+click adds a caret, as in JetBrains IDEs.
            self.selection.with(Range::point(at))
        } else if modifiers.shift {
            let primary = self.selection.primary();
            Selection::single(primary.moved_to(at, true))
        } else {
            match event.click_count {
                2 => Selection::single(carets::select_word(&self.rope, at)),
                3 => Selection::single(carets::select_line(&self.rope, at)),
                _ => Selection::point(at),
            }
        };
        self.dragging = event.click_count < 2;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(at) = self.char_at_point(event.position) {
            let primary = self.selection.primary();
            self.selection = Selection::single(primary.moved_to(at, true));
            self.reveal();
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.dragging = false;
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(self.style.line_height));
        let (dx, dy) = if event.modifiers.shift {
            (delta.y, delta.x)
        } else {
            (delta.x, delta.y)
        };
        self.scroll.y -= dy;
        self.scroll.x -= dx;
        self.clamp_scroll();
        cx.notify();
    }

    // ---- text input -----------------------------------------------------------------------

    fn utf16_range(&self, range: &StdRange<usize>) -> StdRange<usize> {
        utf16_of(&self.rope, range.start)..utf16_of(&self.rope, range.end)
    }

    fn char_range(&self, range: &StdRange<usize>) -> StdRange<usize> {
        char_of_utf16(&self.rope, range.start)..char_of_utf16(&self.rope, range.end)
    }

    /// Replaces `range` (or the composition, or every selection) with `text`.
    fn input(&mut self, range: Option<StdRange<usize>>, text: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let edit = match range.or_else(|| self.marked.clone()) {
            Some(range) => {
                let changes = vec![Change {
                    start: range.start,
                    end: range.end,
                    text: text.to_owned(),
                }];
                Edit {
                    selection: Selection::single(Range::new(range.start, range.end))
                        .after(&changes),
                    changes,
                }
            }
            None => editing::insert(&self.selection, text),
        };
        self.commit(edit, cx);
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: StdRange<usize>,
        adjusted: &mut Option<StdRange<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.char_range(&range);
        adjusted.replace(self.utf16_range(&range));
        Some(self.rope.slice(range).to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let primary = self.selection.primary();
        Some(UTF16Selection {
            range: self.utf16_range(&(primary.start()..primary.end())),
            reversed: primary.head < primary.anchor,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<StdRange<usize>> {
        self.marked.as_ref().map(|r| self.utf16_range(r))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<StdRange<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|r| self.char_range(&r));
        // Typed text with several carets goes to all of them (unless an IME composes).
        let range = if self.selection.len() > 1 && self.marked.is_none() {
            None
        } else {
            range
        };
        self.input(range, text, cx);
        self.marked = None;
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<StdRange<usize>>,
        text: &str,
        selected: Option<StdRange<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let primary = self.selection.primary();
        let range = range
            .map(|r| self.char_range(&r))
            .or_else(|| self.marked.clone())
            .unwrap_or(primary.start()..primary.end());
        let start = range.start;
        self.input(Some(range), text, cx);
        let len = text.chars().count();
        self.marked = (len > 0).then(|| start..start + len);
        if let Some(selected) = selected {
            // `selected` is relative to the composed text, in UTF-16.
            let composed: Vec<char> = text.chars().collect();
            let to_chars = |utf16: usize| {
                let mut units = 0;
                composed
                    .iter()
                    .position(|c| {
                        let before = units;
                        units += c.len_utf16();
                        before >= utf16
                    })
                    .unwrap_or(composed.len())
            };
            self.selection = Selection::single(Range::new(
                start + to_chars(selected.start),
                start + to_chars(selected.end),
            ));
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: StdRange<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.char_range(&range_utf16);
        let layout = self.layout.as_ref()?;
        let (row, _) = self.map.locate(&self.rope, range.start);
        let row_layout = layout.rows.get(row.checked_sub(layout.first_row)?)?;
        let top = layout.bounds.top() + px(row as f32 * self.style.line_height) - self.scroll.y;
        let left = layout.text_left - self.scroll.x;
        Some(Bounds::from_corners(
            point(left + row_layout.x_for(range.start), top),
            point(
                left + row_layout.x_for(range.end),
                top + px(self.style.line_height),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let at = self.char_at_point(point)?;
        Some(utf16_of(&self.rope, at))
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("editor")
            .key_context("Editor")
            .track_focus(&self.focus)
            .size_full()
            .cursor_text()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(EditorElement { view: cx.entity() })
    }
}

/// Draws the visible rows: gutter, current-line highlight, selections, text, carets.
struct EditorElement {
    view: Entity<EditorView>,
}

struct Prepaint {
    layout: Layout,
    gutter: Vec<(Pixels, ShapedLine)>,
    gutter_width: Pixels,
    quads: Vec<gpui::PaintQuad>,
    carets: Vec<gpui::PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // Width changes re-wrap before anything is measured.
        let width_changed = self
            .view
            .read(cx)
            .layout
            .as_ref()
            .is_none_or(|l| l.bounds.size.width != bounds.size.width);
        if width_changed {
            self.view.update(cx, |view, _cx| {
                view.layout = Some(Layout {
                    bounds,
                    text_left: bounds.left(),
                    first_row: 0,
                    rows: Vec::new(),
                });
                view.rewrap();
            });
        }
        let view = self.view.read(cx);
        let style = &view.style;
        let line_height = px(style.line_height);
        let font_size = px(style.font_size);
        let text_font = font(style.font_family.clone());
        let run = |len: usize, color: Hsla| TextRun {
            len,
            font: text_font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let text_system = window.text_system().clone();
        let shape = |text: String, runs: &[TextRun]| {
            text_system.shape_line(SharedString::from(text), font_size, runs, None)
        };

        // Gutter wide enough for the largest line number.
        let digits = view.rope.len_lines().to_string().len().max(2);
        let digit_width = shape("0".into(), &[run(1, style.gutter_foreground)]).width;
        let gutter_width = digit_width * (digits as f32 + 3.0);
        let text_left = bounds.left() + gutter_width + px(6.0);

        let scroll = view.scroll;
        let first_row = (f32::from(scroll.y) / style.line_height).floor().max(0.0) as usize;
        let visible = (f32::from(bounds.size.height) / style.line_height).ceil() as usize + 1;
        let last_row = (first_row + visible).min(view.map.row_count());
        let offset_y = px(first_row as f32 * style.line_height) - scroll.y;
        let caret_line = view.rope.char_to_line(view.selection.primary().head);
        // Syntax highlights of the visible text (the tree catches up with the document).
        let visible_chars = if last_row > first_row {
            view.map.row(&view.rope, first_row).start..view.map.row(&view.rope, last_row - 1).end
        } else {
            0..0
        };
        let len_bytes = view.rope.len_bytes();
        let highlights: Vec<Painted> = view
            .core
            .highlights(
                view.doc,
                view.rope.char_to_byte(visible_chars.start)
                    ..view.rope.char_to_byte(visible_chars.end),
            )
            .into_iter()
            .map(|h| {
                let range = view.rope.byte_to_char(h.range.start.min(len_bytes))
                    ..view.rope.byte_to_char(h.range.end.min(len_bytes));
                (range, token_style(&style.syntax, &h.capture))
            })
            .collect();
        let tab_width = view.map.config().tab_width;

        let mut rows = Vec::with_capacity(last_row.saturating_sub(first_row));
        let mut gutter = Vec::new();
        let mut quads = Vec::new();
        let mut carets = Vec::new();
        for index in first_row..last_row {
            let row = view.map.row(&view.rope, index);
            let top = bounds.top() + offset_y + line_height * (index - first_row) as f32;
            // The row's text with tabs expanded to spaces; offsets map chars to bytes.
            let mut shown = String::new();
            let mut offsets = Vec::with_capacity(row.end - row.start + 1);
            let mut column = row.start_column;
            for c in view.rope.slice(row.start..row.end).chars() {
                offsets.push(shown.len());
                if c == '\t' {
                    let spaces = tab_width.max(1) - column % tab_width.max(1);
                    shown.push_str(&" ".repeat(spaces));
                    column += spaces;
                } else {
                    shown.push(c);
                    column += unicode_width::UnicodeWidthChar::width(c)
                        .unwrap_or(1)
                        .max(1);
                }
            }
            offsets.push(shown.len());
            let mut runs = syntax_runs(&row, &offsets, &highlights, &text_font, style.foreground);
            if let Some(marked) = view.marked.as_ref()
                && marked.start < row.end.max(row.start + 1)
                && marked.end > row.start
            {
                let from = offsets[marked.start.max(row.start) - row.start];
                let to = offsets[marked.end.min(row.end) - row.start];
                let underline = UnderlineStyle {
                    color: Some(style.foreground),
                    thickness: px(1.0),
                    wavy: false,
                };
                runs = [
                    run(from, style.foreground),
                    TextRun {
                        underline: Some(underline),
                        ..run(to - from, style.foreground)
                    },
                    run(shown.len() - to, style.foreground),
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect();
                if runs.is_empty() {
                    runs = vec![run(0, style.foreground)];
                }
            }
            let line = shape(shown, &runs);
            let row_layout = RowLayout {
                start: row.start,
                offsets,
                line,
            };

            if row.line == caret_line {
                quads.push(fill(
                    Bounds::new(
                        point(bounds.left(), top),
                        size(bounds.size.width, line_height),
                    ),
                    style.line_highlight,
                ));
            }
            // Line numbers on a line's first row.
            if row.start == view.rope.line_to_char(row.line) {
                let number = (row.line + 1).to_string();
                let color = if row.line == caret_line {
                    style.active_line_number
                } else {
                    style.gutter_foreground
                };
                let shaped = shape(number.clone(), &[run(number.len(), color)]);
                let x = bounds.left() + gutter_width - shaped.width - digit_width;
                gutter.push((x, shaped));
            }
            let left = text_left - scroll.x;
            for range in view.selection.ranges() {
                let (start, end) = (range.start().max(row.start), range.end());
                // A selection reaching past the row's end covers the line break too.
                let row_end = if end > row.end { row.end } else { end };
                if !range.is_empty() && start <= row_end && end >= row.start && start < end {
                    let mut x1 = left + row_layout.x_for(row_end);
                    if end > row.end {
                        x1 += digit_width * 0.5;
                    }
                    quads.push(fill(
                        Bounds::from_corners(
                            point(left + row_layout.x_for(start), top),
                            point(x1, top + line_height),
                        ),
                        style.selection,
                    ));
                }
                let head_row = view.map.locate(&view.rope, range.head).0;
                if head_row == index {
                    carets.push(fill(
                        Bounds::new(
                            point(left + row_layout.x_for(range.head), top),
                            size(px(2.0), line_height),
                        ),
                        style.caret,
                    ));
                }
            }
            rows.push(row_layout);
        }
        Prepaint {
            layout: Layout {
                bounds,
                text_left,
                first_row,
                rows,
            },
            gutter,
            gutter_width,
            quads,
            carets,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, style, scroll) = {
            let view = self.view.read(cx);
            (view.focus.clone(), view.style.clone(), view.scroll)
        };
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let line_height = px(style.line_height);
        window.paint_quad(fill(bounds, style.background));
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.quads.drain(..) {
                window.paint_quad(quad);
            }
            // Text, clipped to the area right of the gutter.
            let text_bounds = Bounds::from_corners(
                point(bounds.left() + prepaint.gutter_width, bounds.top()),
                bounds.bottom_right(),
            );
            let offset_y = px(prepaint.layout.first_row as f32 * style.line_height) - scroll.y;
            window.with_content_mask(
                Some(gpui::ContentMask {
                    bounds: text_bounds,
                }),
                |window| {
                    for (i, row) in prepaint.layout.rows.iter().enumerate() {
                        let origin = point(
                            prepaint.layout.text_left - scroll.x,
                            bounds.top() + offset_y + line_height * i as f32,
                        );
                        let _ = row.line.paint(origin, line_height, window, cx);
                    }
                    if focus.is_focused(window) {
                        for caret in prepaint.carets.drain(..) {
                            window.paint_quad(caret);
                        }
                    }
                },
            );
            // Gutter on top of a horizontally scrolled text.
            window.paint_quad(fill(
                Bounds::new(
                    bounds.origin,
                    size(prepaint.gutter_width, bounds.size.height),
                ),
                style.gutter_background,
            ));
            let rows: Vec<usize> = (0..prepaint.layout.rows.len()).collect();
            let mut numbers = prepaint.gutter.drain(..);
            for i in rows {
                let row = &prepaint.layout.rows[i];
                let first_of_line = {
                    let view = self.view.read(cx);
                    let line = view.rope.char_to_line(row.start.min(view.rope.len_chars()));
                    view.rope.line_to_char(line) == row.start
                };
                if first_of_line && let Some((x, number)) = numbers.next() {
                    let y = bounds.top() + offset_y + line_height * i as f32;
                    let _ = number.paint(point(x, y), line_height, window, cx);
                }
            }
        });
        let layout = std::mem::replace(
            &mut prepaint.layout,
            Layout {
                bounds,
                text_left: bounds.left(),
                first_row: 0,
                rows: Vec::new(),
            },
        );
        self.view.update(cx, |view, _cx| view.layout = Some(layout));
    }
}

/// A token's colour, bold and italic.
type TokenStyle = (Hsla, bool, bool);
/// Chars of the text and how a highlight paints them.
type Painted = (StdRange<usize>, Option<TokenStyle>);

/// The style of a capture, else of its dotted parents (`function.method` → `function`).
fn token_style(styles: &[(String, Hsla, bool, bool)], capture: &str) -> Option<(Hsla, bool, bool)> {
    let mut name = capture;
    loop {
        if let Some((_, color, bold, italic)) = styles.iter().find(|(c, ..)| c == name) {
            return Some((*color, *bold, *italic));
        }
        name = &name[..name.rfind('.')?];
    }
}

/// Text runs of one row from the highlights: later (inner) captures paint over outer ones.
fn syntax_runs(
    row: &crate::display::Row,
    offsets: &[usize],
    highlights: &[Painted],
    base: &gpui::Font,
    foreground: Hsla,
) -> Vec<TextRun> {
    let chars = row.end - row.start;
    let mut styles: Vec<Option<(Hsla, bool, bool)>> = vec![None; chars];
    for (range, token) in highlights {
        if token.is_none() || range.end <= row.start || range.start >= row.end {
            continue;
        }
        let from = range.start.max(row.start) - row.start;
        let to = range.end.min(row.end) - row.start;
        for slot in &mut styles[from..to] {
            *slot = *token;
        }
    }
    let make = |len: usize, token: Option<(Hsla, bool, bool)>| {
        let mut font = base.clone();
        let mut color = foreground;
        if let Some((c, bold, italic)) = token {
            color = c;
            if bold {
                font.weight = gpui::FontWeight::BOLD;
            }
            if italic {
                font.style = gpui::FontStyle::Italic;
            }
        }
        TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    };
    let mut runs: Vec<TextRun> = Vec::new();
    let mut start = 0;
    while start < chars {
        let token = styles[start];
        let mut end = start + 1;
        while end < chars && styles[end] == token {
            end += 1;
        }
        runs.push(make(offsets[end] - offsets[start], token));
        start = end;
    }
    if runs.is_empty() {
        runs.push(make(0, None));
    }
    runs
}
