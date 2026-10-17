//! UI host (L1, spec, section 20): the workbench window on GPUI (ADR-003).
//!
//! GPUI stays inside this crate: the core is reached only through [`CoreApi`], on the Tokio
//! runtime the caller passes in, while GPUI owns the main thread.

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use std::rc::Rc;
use std::time::Instant;

use kumiko_core::CoreApi;
use tokio::runtime::Handle;

mod a11y;
mod commands;
pub mod editor;
mod input;
mod keys;
pub mod layout;
mod menus;
mod palette;
mod problems;
mod quick_open;
mod state;
mod tabs;
pub mod terminal;
mod tree;
mod workbench;

use editor::EditorProvider;
use workbench::Workbench;

/// Opens the workbench and runs the UI event loop on the current (main) thread until the
/// user quits. Core work is spawned on `runtime`; `started` is when the process started, for
/// the start-up time logged with the first frame.
pub fn run(
    core: CoreApi,
    runtime: Handle,
    started: Instant,
    editors: Rc<dyn EditorProvider>,
    terminals: Rc<dyn terminal::TerminalProvider>,
) {
    let (requests, received) = tokio::sync::mpsc::unbounded_channel();
    // The registered handlers hold the senders; the channel lives as long as the core.
    commands::register(&core, &requests, editors.commands());
    drop(requests);
    let title = core.resources().localize("app.name", &core.locale());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // AccessKit's Windows adapter must subclass the window before it is shown.
            show: false,
            // WM_CLASS on X11, app id on Wayland: lets the desktop match the `.desktop` file.
            app_id: Some("kumiko".to_owned()),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..TitlebarOptions::default()
            }),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |_, cx| {
            cx.new(|cx| Workbench::new(core, runtime, received, started, editors, terminals, cx))
        });
        let window = match opened {
            Ok(window) => window,
            Err(error) => {
                tracing::error!("cannot open the window: {error:#}");
                cx.quit();
                return;
            }
        };
        let shown = window.update(cx, |workbench, window, cx| {
            workbench.attach(window, cx);
            window.activate_window();
        });
        if let Err(error) = shown {
            tracing::error!("cannot show the window: {error:#}");
        }
        // Closing the window ends the IDE.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
