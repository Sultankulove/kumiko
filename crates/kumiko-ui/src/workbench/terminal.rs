//! The Terminal tool window (Alt+F12, MVP criterion 12): sessions as tabs, the active one
//! below. Sessions come from the core (`terminal.opened`), whoever asked for them: the
//! workbench's "New Session" or a plugin (`kumiko.terminal`).

use gpui::{AnyElement, ClickEvent, Context, Window, div, prelude::*, px, rgba};
use kumiko_core::service_api::{ProcessSpec, PtySize};
use kumiko_core::terminals::TerminalId;

use super::{FocusTarget, Workbench};
use crate::layout::ToolWindow;
use crate::terminal::TerminalHandle;

/// A session: its id, title and (once the window had a frame) its view.
pub(super) struct Session {
    pub id: TerminalId,
    pub title: String,
    pub handle: Option<TerminalHandle>,
}

impl Workbench {
    /// A new session: through `kumiko.terminal` (`terminal.new`) when it is installed, which
    /// decides what runs; otherwise the IDE's default shell, until kits install the plugin.
    pub(super) fn new_terminal(&mut self, cx: &mut Context<Self>) {
        if let Ok(id) = kumiko_types::CommandId::new("terminal.new")
            && self.core.commands().contains(&id)
        {
            self.execute(id, Vec::new(), cx);
            return;
        }
        self.default_terminal();
    }

    fn default_terminal(&mut self) {
        let number = self.terminal_counter + 1;
        self.terminal_counter = number;
        let title = if number == 1 {
            self.t("terminal.local")
        } else {
            format!("{} ({number})", self.t("terminal.local"))
        };
        // The core watches the shell on its runtime.
        let _runtime = self.runtime.enter();
        match self.core.open_terminal(
            &title,
            ProcessSpec::default(),
            PtySize {
                cols: 120,
                rows: 30,
            },
        ) {
            Ok(_) => {}
            Err(error) => self.message = Some(error),
        }
    }

    /// A session the core announced: shown and focused.
    pub(super) fn terminal_opened(&mut self, id: TerminalId, title: String) {
        if self.sessions.iter().any(|s| s.id == id) {
            return;
        }
        self.sessions.push(Session {
            id,
            title,
            handle: None,
        });
        self.active_session = self.sessions.len() - 1;
        if !self.layout.is_open(ToolWindow::Terminal) {
            self.layout.toggle(ToolWindow::Terminal);
        }
        self.focus_terminal = true;
    }

    /// A session ended: its tab closes, as IntelliJ does.
    pub(super) fn terminal_closed(&mut self, id: TerminalId) {
        self.sessions.retain(|s| s.id != id);
        self.active_session = self
            .active_session
            .min(self.sessions.len().saturating_sub(1));
        if self.sessions.is_empty() && self.layout.is_open(ToolWindow::Terminal) {
            self.layout.toggle(ToolWindow::Terminal);
            self.focus_request = Some(FocusTarget::Editor);
        }
    }

    /// Opening the window without sessions starts one.
    pub(super) fn ensure_terminal(&mut self, cx: &mut Context<Self>) {
        if self.layout.is_open(ToolWindow::Terminal) && self.sessions.is_empty() {
            self.new_terminal(cx);
        }
        self.focus_terminal = true;
    }

    /// Views for new sessions (they need the window) and the focus a command asked for.
    pub(super) fn ensure_terminal_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (family, size) = {
            let theme = self.editor_theme();
            (theme.font_family, theme.font_size)
        };
        for i in 0..self.sessions.len() {
            if self.sessions[i].handle.is_some() {
                continue;
            }
            let Some(terminal) = self.core.terminal(self.sessions[i].id) else {
                continue;
            };
            let template = self.t("terminal.exited");
            let exited = Box::new(move |code: Option<i32>| match code {
                Some(code) => template.replace("{code}", &code.to_string()),
                None => template.replace("{code}", "?"),
            });
            let options = crate::terminal::TerminalOptions {
                font_family: family.clone(),
                font_size: size,
                exited,
            };
            let handle = self
                .terminals
                .open(&self.core, terminal, options, window, cx);
            self.sessions[i].handle = Some(handle);
        }
        if std::mem::take(&mut self.focus_terminal)
            && self.layout.is_open(ToolWindow::Terminal)
            && let Some(handle) = self
                .sessions
                .get(self.active_session)
                .and_then(|s| s.handle.as_ref())
        {
            handle.focus.focus(window);
        }
    }

    pub(super) fn terminal_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let tabs = self.sessions.iter().enumerate().map(|(i, session)| {
            let active = i == self.active_session;
            let id = session.id;
            div()
                .id(("terminal-tab", i))
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .gap_2()
                .when(active, |d| d.bg(rgba(p.stripe_active)))
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.active_session = i;
                    this.focus_terminal = true;
                    cx.notify();
                }))
                .child(session.title.clone())
                .child(
                    div()
                        .id(("terminal-close", i))
                        .text_color(rgba(p.muted))
                        .hover(|d| d.text_color(rgba(p.foreground)))
                        .child("×")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.core.close_terminal(id);
                        })),
                )
        });
        let add = div()
            .id("terminal-new")
            .px_2()
            .h_full()
            .flex()
            .items_center()
            .text_color(rgba(p.muted))
            .hover(|d| d.text_color(rgba(p.foreground)))
            .child("+")
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.new_terminal(cx);
                cx.notify();
            }));
        let body = self
            .sessions
            .get(self.active_session)
            .and_then(|s| s.handle.as_ref())
            .map_or_else(
                || div().flex_1().into_any_element(),
                |h| {
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(h.view.clone())
                        .into_any_element()
                },
            );
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .text_size(px(12.0))
                    .children(tabs)
                    .child(add),
            )
            .child(body)
            .into_any_element()
    }
}

impl Workbench {
    /// Colours and badges plugins gave files, resolved against the theme.
    pub(super) fn refresh_decorations(&mut self) {
        self.decorations = self
            .core
            .file_decorations()
            .into_iter()
            .filter_map(|(uri, list)| {
                let first = list.into_iter().next()?;
                let color = first.color.as_deref().and_then(|t| self.palette.token(t));
                Some((uri, (color, first.badge)))
            })
            .collect();
    }
}
