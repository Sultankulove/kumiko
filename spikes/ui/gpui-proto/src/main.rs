//! GPUI prototype: a file tree with keyboard navigation and an editor view of the 10 000-line
//! file as a virtualized list of highlighted lines. Benchmarks from `workload`.

use gpui::{
    App, Application, Bounds, Context, FocusHandle, HighlightStyle, KeyDownEvent, ScrollStrategy,
    StyledText, UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size, uniform_list,
};
use workload::{BACKGROUND, Bench, GUTTER, Node, Recorder, Span};

const LINE_HEIGHT: f32 = 20.0;

#[cfg(target_os = "windows")]
const MONOSPACE: &str = "Consolas";
#[cfg(not(target_os = "windows"))]
const MONOSPACE: &str = "DejaVu Sans Mono";

struct Spike {
    lines: Vec<String>,
    spans: Vec<Vec<Span>>,
    nodes: Vec<Node>,
    selected: usize,
    top: usize,
    editor_scroll: UniformListScrollHandle,
    tree_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    recorder: Recorder,
    /// Set by `render`; the editor list may be laid out more than once per frame.
    rendered: bool,
    /// Flips on every typed key; drawn in `probe` mode for the latency probe.
    marker: bool,
    /// AccessKit, attached before the window is shown.
    a11y: Option<a11y_bridge::Bridge>,
}

impl Spike {
    fn new(cx: &mut Context<Self>) -> Self {
        let lines = workload::java_source(workload::LINES);
        let spans = lines.iter().map(|l| workload::highlight(l)).collect();
        Self {
            lines,
            spans,
            nodes: workload::tree(),
            selected: 0,
            top: 0,
            editor_scroll: UniformListScrollHandle::new(),
            tree_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            recorder: Recorder::new("gpui"),
            rendered: false,
            marker: false,
            a11y: None,
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        if let Some(a11y) = &mut self.a11y {
            a11y.select(index);
        }
    }

    fn type_char(&mut self, c: char) {
        let at = (self.top + 5).min(self.lines.len() - 1);
        self.lines[at].insert(0, c);
        self.spans[at] = workload::highlight(&self.lines[at]);
        self.marker = !self.marker;
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => self.select((self.selected + 1).min(self.nodes.len() - 1)),
            "up" => self.select(self.selected.saturating_sub(1)),
            "pagedown" => self.top = (self.top + 40).min(self.lines.len() - 1),
            "pageup" => self.top = self.top.saturating_sub(40),
            _ => {
                let typed = event
                    .keystroke
                    .key_char
                    .as_ref()
                    .and_then(|s| s.chars().next());
                if let Some(c) = typed.filter(|c| !c.is_control()) {
                    self.recorder.key();
                    self.type_char(c);
                }
            }
        }
        self.tree_scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        self.editor_scroll
            .scroll_to_item(self.top, ScrollStrategy::Top);
        cx.notify();
    }

    /// Advances the benchmark by one frame.
    fn bench_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.recorder.bench {
            Bench::Interactive | Bench::Probe => return,
            Bench::Scroll => {
                self.top = (self.top + 1) % (self.lines.len() - 100);
                self.editor_scroll
                    .scroll_to_item(self.top, ScrollStrategy::Top);
            }
            Bench::Type if std::env::var_os("SPIKE_EXTERNAL_KEYS").is_none() => {
                if !self.recorder.key_pending() && self.recorder.frames % 3 == 0 {
                    self.recorder.key();
                    self.type_char('x');
                }
            }
            _ => {}
        }
        if self.recorder.done() {
            self.recorder.finish("");
            cx.quit();
            return;
        }
        // GPUI re-renders a view only when it is notified; a benchmark renders every frame.
        cx.notify();
        window.request_animation_frame();
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rendered = true;
        self.bench_step(window, cx);

        let tree = uniform_list(
            "tree",
            self.nodes.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _window, _cx| {
                range
                    .map(|i| {
                        let node = &this.nodes[i];
                        let icon = if node.dir { "▸ " } else { "  " };
                        div()
                            .id(i)
                            .px_2()
                            .h(px(20.0))
                            .text_size(px(13.0))
                            .bg(rgb(if i == this.selected {
                                0x3a352e
                            } else {
                                0x191816
                            }))
                            .child(format!("{}{icon}{}", "  ".repeat(node.depth), node.name))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.tree_scroll.clone())
        .w(px(280.0))
        .h_full();

        let editor = uniform_list(
            "editor",
            self.lines.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _window, _cx| {
                // Rows are built during layout, the last step of a frame this prototype sees.
                if std::mem::take(&mut this.rendered) {
                    this.recorder.frame();
                }
                range
                    .map(|i| {
                        let highlights = this.spans[i].iter().map(|(range, kind)| {
                            let style = HighlightStyle {
                                color: Some(rgb(kind.rgb()).into()),
                                ..HighlightStyle::default()
                            };
                            (range.clone(), style)
                        });
                        div()
                            .id(i)
                            .flex()
                            .h(px(LINE_HEIGHT))
                            .child(
                                div()
                                    .w(px(64.0))
                                    .text_color(rgb(GUTTER))
                                    .child(format!("{:>6}", i + 1)),
                            )
                            .child(
                                StyledText::new(this.lines[i].clone()).with_highlights(highlights),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.editor_scroll.clone())
        .flex_1()
        .h_full()
        .font_family(MONOSPACE)
        .text_size(px(14.0));

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .flex()
            .size_full()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(0xd8d4cc))
            .relative()
            .child(tree)
            .child(editor)
            .when(self.recorder.bench == Bench::Probe, |root| {
                root.child(
                    div()
                        .absolute()
                        .left(px(workload::MARKER_X))
                        .top(px(workload::MARKER_Y))
                        .size(px(workload::MARKER_SIZE))
                        .bg(rgb(if self.marker { 0xffffff } else { 0x000000 })),
                )
            })
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1920.0), px(1080.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Maximized(bounds)),
                    // AccessKit's Windows adapter must subclass the window before it is shown.
                    show: false,
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Kumiko UI spike: gpui".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(Spike::new),
            )
            .expect("the window opens");
        window
            .update(cx, |spike, window, _cx| {
                // GPUI 0.2.2 panics in `window_handle()` on X11 (`unimplemented!`); the Linux
                // adapter needs no handle anyway.
                #[cfg(windows)]
                let handle = {
                    use raw_window_handle::HasWindowHandle;
                    window.window_handle().ok().map(|h| h.as_raw())
                };
                #[cfg(not(windows))]
                let handle = None;
                let files = spike.nodes.clone();
                spike.a11y = a11y_bridge::Bridge::attach("gpui", handle, files);
                if let Some(a11y) = &mut spike.a11y {
                    a11y.window_focused(true);
                }
                spike.focus.focus(window);
                window.activate_window();
            })
            .expect("the window exists");
        cx.activate(true);
    });
}
