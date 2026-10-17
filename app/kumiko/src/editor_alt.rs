//! A stub editor for the `alt-editor` build profile (spec, section 29, test 5): it shows the
//! text of a document and nothing else. It stands for an editor a distribution brings along:
//! only this file and `app/kumiko/Cargo.toml` know about it, no other crate changes.

use gpui::{
    App, AppContext, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px, rgba,
};
use kumiko_core::CoreApi;
use kumiko_core::documents::Snapshot;
use kumiko_ui::editor::{EditorHandle, EditorProvider, EditorStatus, EditorTheme};
use tokio::runtime::Handle;

pub struct Editors;

struct Stub {
    text: SharedString,
    focus: FocusHandle,
    background: u32,
    foreground: u32,
}

impl Render for Stub {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_2()
            .bg(rgba(self.background))
            .text_color(rgba(self.foreground))
            .text_size(px(14.0))
            .child(self.text.clone())
    }
}

impl Focusable for Stub {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EditorProvider for Editors {
    fn commands(&self) -> &'static [&'static str] {
        &[]
    }

    fn open(
        &self,
        _core: &CoreApi,
        _runtime: &Handle,
        snapshot: &Snapshot,
        theme: EditorTheme,
        _window: &mut Window,
        cx: &mut App,
    ) -> EditorHandle {
        let text = SharedString::from(snapshot.text.to_string());
        let entity = cx.new(|cx| Stub {
            text,
            focus: cx.focus_handle(),
            background: theme.background,
            foreground: theme.foreground,
        });
        let focus = entity.read(cx).focus_handle(cx);
        EditorHandle {
            view: entity.into(),
            focus,
            run: Box::new(|_, _| false),
            goto_line: Box::new(|_, _| {}),
            goto: Box::new(|_, _| {}),
            status: Box::new(|_| EditorStatus {
                line: 1,
                column: 1,
                carets: 1,
                ..EditorStatus::default()
            }),
            set_theme: Box::new(|_, _| {}),
        }
    }
}
