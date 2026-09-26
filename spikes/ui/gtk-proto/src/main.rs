//! GTK4 prototype: a file tree and an editor view of the 10 000-line file, both virtualized
//! `ListView`s; highlighting through Pango markup. Frames are counted on the frame clock.
//! Benchmarks from `workload`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use workload::{BACKGROUND, Bench, GUTTER, Node, Recorder, Span};

struct Model {
    lines: Vec<String>,
    spans: Vec<Vec<Span>>,
    top: u32,
    recorder: Recorder,
}

fn markup(index: usize, line: &str, spans: &[Span]) -> String {
    let mut out = format!("<span foreground='#{GUTTER:06x}'>{:>6}  </span>", index + 1);
    for (range, kind) in spans {
        let text = glib::markup_escape_text(&line[range.clone()]);
        out.push_str(&format!(
            "<span foreground='#{:06x}'>{text}</span>",
            kind.rgb()
        ));
    }
    out
}

fn list(items: impl Iterator<Item = String>) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(&item);
    }
    list
}

fn tree_view(nodes: &[Node]) -> gtk::ListView {
    let labels = list(nodes.iter().map(|node| {
        let icon = if node.dir { "▸ " } else { "  " };
        format!("{}{icon}{}", "  ".repeat(node.depth), node.name)
    }));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let text = item
            .item()
            .and_downcast::<gtk::StringObject>()
            .expect("a string");
        let label = item.child().and_downcast::<gtk::Label>().expect("a label");
        label.set_text(&text.string());
    });
    gtk::ListView::new(Some(gtk::SingleSelection::new(Some(labels))), Some(factory))
}

fn editor_view(model: &Rc<RefCell<Model>>, rows: &gtk::StringList) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let label = gtk::Label::builder().xalign(0.0).use_markup(true).build();
        item.set_child(Some(&label));
    });
    let bound = model.clone();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let index = item.position() as usize;
        let model = bound.borrow();
        let label = item.child().and_downcast::<gtk::Label>().expect("a label");
        label.set_markup(&markup(index, &model.lines[index], &model.spans[index]));
    });
    gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(rows.clone()))),
        Some(factory),
    )
}

fn build(app: &gtk::Application) {
    let lines = workload::java_source(workload::LINES);
    let spans = lines.iter().map(|l| workload::highlight(l)).collect();
    let model = Rc::new(RefCell::new(Model {
        lines,
        spans,
        top: 0,
        recorder: Recorder::new("gtk4"),
    }));
    let rows = list((0..workload::LINES).map(|i| i.to_string()));

    let tree = tree_view(&workload::tree());
    let editor = editor_view(&model, &rows);
    let css = gtk::CssProvider::new();
    css.load_from_string(&format!(
        "listview.editor {{ font-family: monospace; font-size: 14px; background: #{BACKGROUND:06x}; }}"
    ));
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(&editor),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    editor.add_css_class("editor");

    let tree_scroll = gtk::ScrolledWindow::builder()
        .child(&tree)
        .width_request(280)
        .build();
    let editor_scroll = gtk::ScrolledWindow::builder()
        .child(&editor)
        .hexpand(true)
        .build();
    let paned = gtk::Paned::builder()
        .start_child(&tree_scroll)
        .end_child(&editor_scroll)
        .position(280)
        .build();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Kumiko UI spike: GTK4")
        .default_width(1920)
        .default_height(1080)
        .child(&paned)
        .build();

    // The benchmark advances one step per frame, in the frame clock's update phase.
    let ticking = model.clone();
    window.add_tick_callback(move |_, _clock| {
        let mut model = ticking.borrow_mut();
        match model.recorder.bench {
            Bench::Interactive => return glib::ControlFlow::Continue,
            Bench::Scroll => {
                model.top = (model.top + 1) % (workload::LINES as u32 - 100);
                let top = model.top;
                // Scrolling binds rows right away, and binding reads the model.
                drop(model);
                editor.scroll_to(top, gtk::ListScrollFlags::NONE, None);
            }
            Bench::Type if std::env::var_os("SPIKE_EXTERNAL_KEYS").is_none() => {
                if !model.recorder.key_pending() && model.recorder.frames % 3 == 0 {
                    model.recorder.key();
                    let at = (model.top + 5) as usize;
                    model.lines[at].insert(0, 'x');
                    model.spans[at] = workload::highlight(&model.lines[at]);
                    let item = at.to_string();
                    drop(model);
                    // Replacing the item rebinds its row.
                    rows.splice(at as u32, 1, &[item.as_str()]);
                }
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
    window.present();

    // A frame counts once it is painted.
    let painted = model.clone();
    let app = app.clone();
    if let Some(clock) = window.frame_clock() {
        clock.connect_after_paint(move |_| {
            let mut model = painted.borrow_mut();
            model.recorder.frame();
            if model.recorder.done() {
                model.recorder.finish("");
                app.quit();
            }
        });
    }
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.kumiko.spike.gtk")
        .build();
    app.connect_activate(build);
    app.run_with_args::<&str>(&[])
}
