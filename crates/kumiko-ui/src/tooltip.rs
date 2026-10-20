//! The tooltip of buttons that show only an icon.

use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgba};

pub(crate) struct Tooltip {
    pub text: SharedString,
    /// Colours of the palette, `0xRRGGBBAA`.
    pub background: u32,
    pub foreground: u32,
    pub border: u32,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(rgba(self.background))
            .text_color(rgba(self.foreground))
            .border_1()
            .border_color(rgba(self.border))
            .text_size(px(12.0))
            .shadow_md()
            .child(self.text.clone())
    }
}
