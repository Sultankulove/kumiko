//! The terminal view the workbench uses: `kumiko-terminal-view`, plugged in here so the UI
//! crates do not depend on each other (like the editor).

use std::sync::Arc;

use gpui::{App, AppContext, Focusable, Window};
use kumiko_core::CoreApi;
use kumiko_core::terminals::Terminal;
use kumiko_terminal_view::colors::Palette;
use kumiko_terminal_view::view::{TerminalStyle, TerminalView};
use kumiko_ui::terminal::{TerminalHandle, TerminalOptions, TerminalProvider};

pub struct Terminals;

impl TerminalProvider for Terminals {
    fn open(
        &self,
        core: &CoreApi,
        terminal: Arc<Terminal>,
        options: TerminalOptions,
        _window: &mut Window,
        cx: &mut App,
    ) -> TerminalHandle {
        let style = TerminalStyle {
            font_family: options.font_family.into(),
            font_size: options.font_size,
            palette: Palette::default(),
        };
        let entity =
            cx.new(|cx| TerminalView::new(core.clone(), terminal, style, options.exited, cx));
        let focus = entity.read(cx).focus_handle(cx);
        TerminalHandle {
            view: entity.into(),
            focus,
        }
    }
}
