//! The editor the workbench uses: `kumiko-editor-view`, plugged in here so the UI crates do
//! not depend on each other (spec, section 12: the editor is replaced at build time).

use gpui::{App, AppContext, Focusable, Window, rgba};
use kumiko_core::CoreApi;
use kumiko_core::documents::Snapshot;
use kumiko_editor_view::view::{COMMANDS, EditorStyle, EditorView};
use kumiko_ui::editor::{EditorHandle, EditorProvider, EditorStatus, EditorTheme};
use tokio::runtime::Handle;

pub struct Editors;

fn style(theme: &EditorTheme) -> EditorStyle {
    EditorStyle {
        background: rgba(theme.background).into(),
        foreground: rgba(theme.foreground).into(),
        gutter_background: rgba(theme.gutter).into(),
        gutter_foreground: rgba(theme.line_number).into(),
        active_line_number: rgba(theme.line_number_active).into(),
        line_highlight: rgba(theme.line_highlight).into(),
        selection: rgba(theme.selection).into(),
        caret: rgba(theme.caret).into(),
        find_match: rgba(theme.find_match).into(),
        syntax: theme
            .syntax
            .iter()
            .map(|(capture, color, bold, italic)| {
                (capture.clone(), rgba(*color).into(), *bold, *italic)
            })
            .collect(),
        font_family: theme.font_family.clone().into(),
        font_size: theme.font_size,
        line_height: theme.line_height,
    }
}

impl EditorProvider for Editors {
    fn commands(&self) -> &'static [&'static str] {
        COMMANDS
    }

    fn open(
        &self,
        core: &CoreApi,
        runtime: &Handle,
        snapshot: &Snapshot,
        theme: EditorTheme,
        _window: &mut Window,
        cx: &mut App,
    ) -> EditorHandle {
        let entity = cx
            .new(|cx| EditorView::new(core.clone(), runtime.clone(), snapshot, style(&theme), cx));
        let focus = entity.read(cx).focus_handle(cx);
        let (run, goto, status, themed) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        EditorHandle {
            view: entity.into(),
            focus,
            run: Box::new(move |command, cx| run.update(cx, |view, cx| view.run(command, cx))),
            goto_line: Box::new(move |line, cx| {
                goto.update(cx, |view, cx| view.goto_line(line, cx))
            }),
            status: Box::new(move |cx| {
                let view = status.read(cx);
                let (line, column) = view.caret_position();
                EditorStatus {
                    line,
                    column,
                    carets: view.caret_count(),
                    message: view.message.clone(),
                }
            }),
            set_theme: Box::new(move |theme, cx| {
                themed.update(cx, |view, cx| view.set_style(style(&theme), cx))
            }),
        }
    }
}
