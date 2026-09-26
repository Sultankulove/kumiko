//! iced prototype: a file tree with keyboard navigation and an editor view of the 10 000-line
//! file, drawn on a canvas (iced renders text with cosmic-text). Benchmarks from `workload`.

use std::cell::RefCell;
use std::rc::Rc;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Text};
use iced::widget::{column, container, row, scrollable, text};
use iced::{
    Color, Element, Font, Length, Point, Rectangle, Renderer, Size, Subscription, Task, Theme,
    mouse,
};
use workload::{BACKGROUND, Bench, GUTTER, Kind, Node, Recorder, Span};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
/// Advance of the monospace font, as a share of its size (DejaVu Sans Mono: 0.602).
const ADVANCE: f32 = 0.602 * FONT_SIZE;
const GUTTER_WIDTH: f32 = 64.0;
const TREE_WIDTH: f32 = 280.0;

fn color(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

struct Spike {
    lines: Vec<String>,
    spans: Vec<Vec<Span>>,
    nodes: Vec<Node>,
    selected: usize,
    top: usize,
    recorder: Rc<RefCell<Recorder>>,
    /// Flips on every typed key; drawn in `probe` mode for the latency probe.
    marker: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Frame,
    Key(keyboard::Event),
}

impl Spike {
    fn new() -> Self {
        let lines = workload::java_source(workload::LINES);
        let spans = lines.iter().map(|l| workload::highlight(l)).collect();
        Self {
            lines,
            spans,
            nodes: workload::tree(),
            selected: 0,
            top: 0,
            recorder: Rc::new(RefCell::new(Recorder::new("iced"))),
            marker: false,
        }
    }

    fn type_char(&mut self, c: char) {
        let at = (self.top + 5).min(self.lines.len() - 1);
        self.lines[at].insert(0, c);
        self.spans[at] = workload::highlight(&self.lines[at]);
        self.marker = !self.marker;
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Frame => {
                let bench = self.recorder.borrow().bench;
                match bench {
                    Bench::Scroll => self.top = (self.top + 1) % (self.lines.len() - 100),
                    Bench::Type if std::env::var_os("SPIKE_EXTERNAL_KEYS").is_none() => {
                        let (pending, frames) = {
                            let r = self.recorder.borrow();
                            (r.key_pending(), r.frames)
                        };
                        if !pending && frames % 3 == 0 {
                            self.recorder.borrow_mut().key();
                            self.type_char('x');
                        }
                    }
                    _ => {}
                }
                if self.recorder.borrow().done() {
                    self.recorder.borrow().finish("");
                    return iced::exit();
                }
            }
            Message::Key(keyboard::Event::KeyPressed { key, text, .. }) => match key {
                Key::Named(Named::ArrowDown) => {
                    self.selected = (self.selected + 1).min(self.nodes.len() - 1);
                    announce(self.selected);
                }
                Key::Named(Named::ArrowUp) => {
                    self.selected = self.selected.saturating_sub(1);
                    announce(self.selected);
                }
                Key::Named(Named::PageDown) => self.top = (self.top + 40).min(self.lines.len() - 1),
                Key::Named(Named::PageUp) => self.top = self.top.saturating_sub(40),
                _ => {
                    if let Some(c) = text
                        .and_then(|t| t.chars().next())
                        .filter(|c| !c.is_control())
                    {
                        self.recorder.borrow_mut().key();
                        self.type_char(c);
                    }
                }
            },
            Message::Key(_) => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let rows = self.nodes.iter().enumerate().map(|(i, node)| {
            let icon = if node.dir { "▸ " } else { "  " };
            let label = text(format!("{}{icon}{}", "  ".repeat(node.depth), node.name)).size(13);
            let selected = i == self.selected;
            container(label)
                .width(Length::Fill)
                .padding([1, 6])
                .style(move |_theme: &Theme| {
                    container::background(if selected {
                        color(0x3a352e)
                    } else {
                        color(0x191816)
                    })
                })
                .into()
        });
        let editor = Canvas::new(Editor { spike: self })
            .width(Length::Fill)
            .height(Length::Fill);
        // Start-up breakdown: `SPIKE_VIEW=editor` drops the tree, `empty` also the text.
        match std::env::var("SPIKE_VIEW").as_deref() {
            Ok("editor" | "empty") => editor.into(),
            _ => {
                let tree = scrollable(column(rows))
                    .width(TREE_WIDTH)
                    .height(Length::Fill);
                row![tree, editor].into()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let keys = keyboard::listen().map(Message::Key);
        if matches!(
            self.recorder.borrow().bench,
            Bench::Interactive | Bench::Probe
        ) {
            keys
        } else {
            Subscription::batch([keys, iced::window::frames().map(|_| Message::Frame)])
        }
    }
}

/// The editor view: visible lines only, one text run per highlighted span.
struct Editor<'a> {
    spike: &'a Spike,
}

impl canvas::Program<Message> for Editor<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let spike = self.spike;
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), color(BACKGROUND));
        let empty = std::env::var("SPIKE_VIEW").as_deref() == Ok("empty");
        let visible = if empty {
            0
        } else {
            (bounds.height / LINE_HEIGHT).ceil() as usize + 1
        };
        let end = (spike.top + visible).min(spike.lines.len());
        for (row, index) in (spike.top..end).enumerate() {
            let y = row as f32 * LINE_HEIGHT;
            frame.fill_text(Text {
                content: format!("{:>6}", index + 1),
                position: Point::new(4.0, y),
                color: color(GUTTER),
                size: FONT_SIZE.into(),
                font: Font::MONOSPACE,
                ..Text::default()
            });
            let line = &spike.lines[index];
            for (range, kind) in &spike.spans[index] {
                if *kind == Kind::Plain && line[range.clone()].trim().is_empty() {
                    continue;
                }
                let column = line[..range.start].chars().count() as f32;
                frame.fill_text(Text {
                    content: line[range.clone()].to_owned(),
                    position: Point::new(GUTTER_WIDTH + column * ADVANCE, y),
                    color: color(kind.rgb()),
                    size: FONT_SIZE.into(),
                    font: Font::MONOSPACE,
                    ..Text::default()
                });
            }
        }
        // iced redraws only when the picture changes; while benchmarking, a frame counter in the
        // corner changes every frame, like a blinking caret does in a real editor.
        let bench = spike.recorder.borrow().bench;
        if matches!(bench, Bench::Start | Bench::Scroll | Bench::Type) {
            frame.fill_text(Text {
                content: format!("{}", spike.recorder.borrow().frames),
                position: Point::new(bounds.width - 80.0, 4.0),
                color: color(GUTTER),
                size: FONT_SIZE.into(),
                font: Font::MONOSPACE,
                ..Text::default()
            });
        }
        if bench == Bench::Probe {
            let at = Point::new(workload::MARKER_X - TREE_WIDTH, workload::MARKER_Y);
            let size = Size::new(workload::MARKER_SIZE, workload::MARKER_SIZE);
            frame.fill_rectangle(
                at,
                size,
                if spike.marker {
                    Color::WHITE
                } else {
                    Color::BLACK
                },
            );
        }
        // The last thing of the frame this prototype can observe.
        spike.recorder.borrow_mut().frame();
        vec![frame.into_geometry()]
    }
}

thread_local! {
    /// The accessibility adapter lives on the event-loop thread, where `update` runs too.
    static A11Y: RefCell<Option<a11y_bridge::Bridge>> = const { RefCell::new(None) };
}

/// Tells assistive technologies that the selection (and focus) moved.
fn announce(selected: usize) {
    A11Y.with_borrow_mut(|bridge| {
        if let Some(bridge) = bridge {
            bridge.select(selected);
        }
    });
}

/// `SPIKE_NO_A11Y=1` opens the window visible, without AccessKit (start-up comparison).
fn no_a11y() -> bool {
    std::env::var_os("SPIKE_NO_A11Y").is_some()
}

/// The window starts hidden: AccessKit's Windows adapter must subclass it before it is shown.
fn boot() -> (Spike, Task<Message>) {
    use iced::window;
    let spike = Spike::new();
    if no_a11y() {
        return (spike, Task::none());
    }
    let files = spike.nodes.clone();
    let attach = window::oldest().then(move |id| {
        let Some(id) = id else {
            return Task::none();
        };
        let files = files.clone();
        window::run(id, move |window| {
            let handle = window.window_handle().map(|h| h.as_raw());
            if let Ok(handle) = handle {
                let bridge = a11y_bridge::Bridge::attach("iced", Some(handle), files);
                A11Y.with_borrow_mut(|slot| *slot = bridge);
                A11Y.with_borrow_mut(|bridge| {
                    if let Some(bridge) = bridge {
                        bridge.window_focused(true);
                    }
                });
            }
        })
        .then(move |()| window::set_mode(id, window::Mode::Windowed))
        .chain(window::maximize(id, true))
        .chain(window::gain_focus(id))
    });
    (spike, attach)
}

fn main() -> iced::Result {
    iced::application(boot, Spike::update, Spike::view)
        .subscription(Spike::subscription)
        .title("Kumiko UI spike: iced")
        .theme(|_: &Spike| Theme::Dark)
        .window(iced::window::Settings {
            maximized: true,
            visible: no_a11y(),
            ..iced::window::Settings::default()
        })
        .run()
}
