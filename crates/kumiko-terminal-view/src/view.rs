//! A terminal on GPUI: a reader thread feeds the shell's bytes to alacritty_terminal's
//! emulator; the element draws its grid (backgrounds, text runs, cursor) and the view turns
//! keys, paste and the wheel into bytes and scrolling. The size follows the element.

use std::io::Read;
use std::sync::{Arc, Mutex, MutexGuard};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb};
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, FocusHandle, Focusable,
    GlobalElementId, Hsla, IntoElement, KeyDownEvent, LayoutId, Pixels, Render, ScrollWheelEvent,
    ShapedLine, SharedString, Style, TextRun, Window, div, fill, font, point, prelude::*, px,
    relative, rgb, size,
};
use kumiko_core::CoreApi;
use kumiko_core::service_api::PtySize;
use kumiko_core::terminals::Terminal;

use crate::colors::Palette;
use crate::keys;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Grid size for the emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Answers the emulator wants to send (cursor position, device attributes) go to the shell.
#[derive(Clone)]
struct Listener {
    terminal: Arc<Terminal>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        if let TermEvent::PtyWrite(text) = event {
            self.terminal.write(text.as_bytes());
        }
    }
}

/// What the element and the reader thread share.
struct Shared {
    term: Mutex<Term<Listener>>,
    size: Mutex<GridSize>,
    terminal: Arc<Terminal>,
}

/// Font and colours of terminals.
#[derive(Debug, Clone)]
pub struct TerminalStyle {
    pub font_family: SharedString,
    pub font_size: f32,
    pub palette: Palette,
}

pub struct TerminalView {
    shared: Arc<Shared>,
    style: TerminalStyle,
    focus: FocusHandle,
    /// The shell's exit code once it ended.
    exited: Option<Option<i32>>,
    /// "Process finished with exit code {code}", localized by the provider.
    exited_text: Box<dyn Fn(Option<i32>) -> String>,
}

impl TerminalView {
    pub fn new(
        _core: CoreApi,
        terminal: Arc<Terminal>,
        style: TerminalStyle,
        exited_text: Box<dyn Fn(Option<i32>) -> String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize { cols: 80, rows: 24 };
        let listener = Listener {
            terminal: terminal.clone(),
        };
        let term = Term::new(Config::default(), &size, listener);
        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            size: Mutex::new(size),
            terminal: terminal.clone(),
        });
        // The shell's output, parsed on a thread of its own; the view redraws after it.
        let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel::<()>();
        if let Some(mut output) = terminal.take_output() {
            let shared = shared.clone();
            let spawned = std::thread::Builder::new()
                .name("kumiko-terminal".into())
                .spawn(move || {
                    let mut parser: Processor = Processor::new();
                    let mut buffer = vec![0u8; 64 * 1024];
                    while let Ok(n) = output.read(&mut buffer) {
                        if n == 0 {
                            break;
                        }
                        parser.advance(&mut *lock(&shared.term), &buffer[..n]);
                        if changed.send(()).is_err() {
                            break;
                        }
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!("no terminal reader: {error}");
            }
        }
        cx.spawn(async move |this, cx| {
            while changes.recv().await.is_some() {
                while changes.try_recv().is_ok() {}
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
        let waiting = terminal.clone();
        cx.spawn(async move |this, cx| {
            let code = waiting.wait().await;
            let _ = this.update(cx, |view, cx| {
                view.exited = Some(code);
                cx.notify();
            });
        })
        .detach();
        Self {
            shared,
            style,
            focus: cx.focus_handle(),
            exited: None,
            exited_text,
        }
    }

    pub fn set_style(&mut self, style: TerminalStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.shared.terminal
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        let bracketed = lock(&self.shared.term)
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let terminal = &self.shared.terminal;
        if bracketed {
            terminal.write(b"\x1b[200~");
            terminal.write(text.as_bytes());
            terminal.write(b"\x1b[201~");
        } else {
            terminal.write(text.as_bytes());
        }
    }

    /// The visible text, for copying (no selection yet: the whole screen).
    fn screen_text(&self) -> String {
        let term = lock(&self.shared.term);
        let content = term.renderable_content();
        let mut lines: Vec<String> = Vec::new();
        let mut current = i32::MIN;
        for cell in content.display_iter {
            if cell.point.line.0 != current {
                current = cell.point.line.0;
                lines.push(String::new());
            }
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                && let Some(line) = lines.last_mut()
            {
                line.push(cell.c);
            }
        }
        lines
            .iter()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let m = stroke.modifiers;
        let key = stroke.key.as_str();
        // Clipboard: Ctrl+Shift+C / Ctrl+Shift+V, and Ctrl+V as in IntelliJ's terminal.
        if m.control && m.shift && key == "c" {
            cx.write_to_clipboard(ClipboardItem::new_string(self.screen_text()));
            cx.stop_propagation();
            return;
        }
        if m.control && key == "v" {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // Keys that stay with the IDE: Escape (back to the editor), shortcuts with
        // Ctrl+Shift, Alt+digit tool windows, Alt+F12.
        let ide = key == "escape"
            || (m.control && m.shift)
            || m.platform
            || (m.alt
                && (key.len() == 1 && key.chars().all(|c| c.is_ascii_digit()) || key == "f12"));
        if ide || self.exited.is_some() {
            return;
        }
        let app_cursor = lock(&self.shared.term)
            .mode()
            .contains(TermMode::APP_CURSOR);
        let Some(bytes) = keys::bytes(
            key,
            stroke.key_char.as_deref(),
            m.control,
            m.alt,
            m.shift,
            app_cursor,
        ) else {
            return;
        };
        // Typing shows the bottom again.
        lock(&self.shared.term).scroll_display(Scroll::Bottom);
        self.shared.terminal.write(&bytes);
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = px(self.style.font_size * 1.3);
        let lines = match event.delta {
            gpui::ScrollDelta::Lines(delta) => delta.y.round() as i32 * 3,
            gpui::ScrollDelta::Pixels(delta) => {
                (f32::from(delta.y) / f32::from(line_height)).round() as i32
            }
        };
        if lines != 0 {
            lock(&self.shared.term).scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let background = to_hsla(self.style.palette.background);
        let exited = self.exited.map(|code| (self.exited_text)(code));
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|view, _, window, _cx| {
                    window.focus(&view.focus);
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(div().flex_1().overflow_hidden().child(TerminalElement {
                shared: self.shared.clone(),
                style: self.style.clone(),
                focused,
            }))
            .children(exited.map(|text| {
                div()
                    .px_2()
                    .text_color(to_hsla(self.style.palette.ansi[8]))
                    .text_size(px(self.style.font_size))
                    .child(text)
            }))
    }
}

fn to_hsla(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}

struct TerminalElement {
    shared: Arc<Shared>,
    style: TerminalStyle,
    focused: bool,
}

struct Prepaint {
    rows: Vec<ShapedLine>,
    quads: Vec<gpui::PaintQuad>,
    cursor: Option<gpui::PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
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
    ) -> (LayoutId, ()) {
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
        _state: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Prepaint {
        let palette = &self.style.palette;
        let font_size = px(self.style.font_size);
        let line_height = px((self.style.font_size * 1.3).round());
        let base = font(self.style.font_family.clone());
        let text_system = window.text_system().clone();
        let probe = text_system.shape_line(
            "M".into(),
            font_size,
            &[TextRun {
                len: 1,
                font: base.clone(),
                color: to_hsla(palette.foreground),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let cell_width = probe.width.max(px(1.0));

        // The grid follows the element: the emulator and the PTY get the new size.
        let wanted = GridSize {
            cols: ((f32::from(bounds.size.width) / f32::from(cell_width)).floor() as usize).max(2),
            rows: ((f32::from(bounds.size.height) / f32::from(line_height)).floor() as usize)
                .max(1),
        };
        {
            let mut current = lock(&self.shared.size);
            if *current != wanted {
                *current = wanted;
                lock(&self.shared.term).resize(wanted);
                self.shared.terminal.resize(PtySize {
                    cols: wanted.cols as u16,
                    rows: wanted.rows as u16,
                });
            }
        }

        let term = lock(&self.shared.term);
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let mut texts: Vec<(String, Vec<TextRun>)> = vec![(String::new(), Vec::new()); wanted.rows];
        let mut quads = Vec::new();
        for cell in content.display_iter {
            let row = cell.point.line.0 + offset;
            if row < 0 || row as usize >= wanted.rows {
                continue;
            }
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let col = cell.point.column.0;
            let (mut fg, mut bg) = (palette.resolve(cell.fg), palette.resolve(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if bg != palette.background {
                quads.push(fill(
                    Bounds::new(
                        point(
                            bounds.left() + cell_width * col as f32,
                            bounds.top() + line_height * row as f32,
                        ),
                        size(cell_width, line_height),
                    ),
                    to_hsla(bg),
                ));
            }
            let (text, runs) = &mut texts[row as usize];
            // Columns before this cell that the iterator skipped are blanks.
            let mut ch = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            if ch == '\0' {
                ch = ' ';
            }
            let mut font = base.clone();
            if cell.flags.contains(Flags::BOLD) {
                font.weight = gpui::FontWeight::BOLD;
            }
            if cell.flags.contains(Flags::ITALIC) {
                font.style = gpui::FontStyle::Italic;
            }
            let color = to_hsla(fg);
            let len = ch.len_utf8();
            text.push(ch);
            match runs.last_mut() {
                Some(run) if run.color == color && run.font == font => run.len += len,
                _ => runs.push(TextRun {
                    len,
                    font,
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }),
            }
        }
        let cursor = (content.cursor.shape != CursorShape::Hidden).then(|| {
            let row = content.cursor.point.line.0 + offset;
            let origin = point(
                bounds.left() + cell_width * content.cursor.point.column.0 as f32,
                bounds.top() + line_height * row as f32,
            );
            let color = to_hsla(palette.cursor);
            if self.focused {
                fill(
                    Bounds::new(origin, size(cell_width, line_height)),
                    color.opacity(0.7),
                )
            } else {
                gpui::outline(
                    Bounds::new(origin, size(cell_width, line_height)),
                    color,
                    gpui::BorderStyle::Solid,
                )
            }
        });
        drop(term);
        let rows = texts
            .into_iter()
            .map(|(text, runs)| {
                text_system.shape_line(SharedString::from(text), font_size, &runs, None)
            })
            .collect();
        Prepaint {
            rows,
            quads,
            cursor,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.quads.drain(..) {
                window.paint_quad(quad);
            }
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
            for (i, line) in prepaint.rows.iter().enumerate() {
                let origin = point(
                    bounds.left(),
                    bounds.top() + prepaint.line_height * i as f32,
                );
                let _ = line.paint(origin, prepaint.line_height, window, cx);
            }
        });
    }
}
