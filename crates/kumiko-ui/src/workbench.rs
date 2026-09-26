//! The workbench window (spec, section 20) in the Classic UI arrangement of JetBrains IDEs:
//! menu bar, toolbar, tool-window stripes, tool windows, editor area, status bar. The geometry
//! comes from [`LayoutState::regions`]; this module only draws it and routes input to commands.

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, KeyDownEvent, MouseButton, Window, div,
    prelude::*, px, rgba,
};
use kumiko_core::CoreApi;
use kumiko_core::config::keymap::{KeySequence, Resolution};
use kumiko_types::CommandId;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::a11y::{AccessNode, AccessTree, Bridge};
use crate::commands::UiCommand;
use crate::keys;
use crate::layout::{LayoutState, Rect, Side, ToolWindow};
use crate::menus::{self, CommandLookup, Menu};
use crate::palette::Palette;

#[cfg(windows)]
const UI_FONT: &str = "Segoe UI";
#[cfg(not(windows))]
const UI_FONT: &str = "Noto Sans";
const UI_TEXT: f32 = 13.0;

/// Accessible node ids of the workbench regions.
mod node {
    pub const WINDOW: u64 = 1;
    pub const MENU_BAR: u64 = 2;
    pub const TOOLBAR: u64 = 3;
    pub const PROJECT: u64 = 4;
    pub const EDITOR: u64 = 5;
    pub const BOTTOM: u64 = 6;
    pub const STATUS: u64 = 7;
    /// Top-level menus follow, one id each.
    pub const MENUS: u64 = 100;
}

pub struct Workbench {
    core: CoreApi,
    runtime: Handle,
    locale: String,
    palette: Palette,
    layout: LayoutState,
    focus: FocusHandle,
    /// Chords typed so far of a multi-key binding.
    pending: Vec<kumiko_core::config::keymap::KeyChord>,
    /// The main menu, rebuilt when it opens; the index of the open one.
    menus: Vec<Menu>,
    open_menu: Option<usize>,
    /// The last message for the status bar (e.g. a failed command).
    message: Option<String>,
    a11y: Option<Bridge>,
}

impl Workbench {
    pub fn new(
        core: CoreApi,
        runtime: Handle,
        requests: UnboundedReceiver<UiCommand>,
        cx: &mut Context<Self>,
    ) -> Self {
        let locale = core.locale();
        let palette = palette(&core);
        let focus = cx.focus_handle();
        // Commands handled on the core's side arrive here, on the UI thread.
        cx.spawn(async move |this, cx| {
            let mut requests = requests;
            while let Some(command) = requests.recv().await {
                let applied = this.update(cx, |workbench, cx| workbench.apply(command, cx));
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut workbench = Self {
            core,
            runtime,
            locale,
            palette,
            layout: LayoutState::default(),
            focus,
            pending: Vec::new(),
            menus: Vec::new(),
            open_menu: None,
            message: None,
            a11y: None,
        };
        workbench.menus = workbench.build_menus();
        workbench
    }

    /// Hooks up accessibility and keyboard focus once the window exists, before it is shown.
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // GPUI 0.2.2 panics in `window_handle()` on X11 (`unimplemented!`); the Linux
        // adapter needs no handle anyway.
        #[cfg(windows)]
        let handle = {
            use raw_window_handle::HasWindowHandle;
            window.window_handle().ok().map(|h| h.as_raw())
        };
        #[cfg(not(windows))]
        let handle = None;
        self.a11y = Bridge::attach(handle, self.access_tree());
        cx.observe_window_activation(window, |this, window, _cx| {
            let active = window.is_window_active();
            if let Some(a11y) = &mut this.a11y {
                a11y.window_focused(active);
            }
        })
        .detach();
        self.focus.focus(window);
    }

    fn t(&self, key: &str) -> String {
        self.core.resources().localize(key, &self.locale)
    }

    fn apply(&mut self, command: UiCommand, cx: &mut Context<Self>) {
        match command {
            UiCommand::Toggle(window) => self.layout.toggle(window),
            UiCommand::Quit => cx.quit(),
        }
        self.publish_context();
        cx.notify();
    }

    /// Context keys for when-expressions: which tool windows are visible.
    fn publish_context(&self) {
        let context = self.core.commands().context();
        for window in ToolWindow::ALL {
            let key = format!(
                "workbench.{}.visible",
                window.title_key().trim_start_matches("toolWindow.")
            );
            context.set(key, Value::Bool(self.layout.is_open(window)));
        }
    }

    /// Runs a command on the core's runtime; a failure shows in the status bar.
    fn execute(&mut self, id: CommandId, cx: &mut Context<Self>) {
        self.open_menu = None;
        let core = self.core.clone();
        let task = self
            .runtime
            .spawn(async move { core.commands().execute(&id, Vec::new()).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = task.await {
                let _ = this.update(cx, |workbench, cx| {
                    workbench.message = Some(error.to_string());
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        let Some(chord) = keys::chord(
            modifiers.control,
            modifiers.alt,
            modifiers.shift,
            modifiers.platform,
            &stroke.key,
        ) else {
            return;
        };
        if chord.key == "escape" && self.open_menu.is_some() {
            self.open_menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        self.pending.push(chord);
        let pressed = KeySequence(self.pending.clone());
        let keymap = self.core.config().keymap();
        match keymap.resolve(&pressed, self.core.commands().context()) {
            Resolution::Command(id) => {
                self.pending.clear();
                cx.stop_propagation();
                self.execute(id, cx);
            }
            Resolution::Pending => cx.stop_propagation(),
            Resolution::None => self.pending.clear(),
        }
    }

    fn build_menus(&self) -> Vec<Menu> {
        let registry = self.core.plugins();
        let context = self.core.commands().context();
        let contributions: Vec<(String, CommandId)> = registry
            .menus()
            .filter(|(_, m)| {
                m.when.as_deref().is_none_or(|w| {
                    kumiko_core::service_api::when::WhenExpr::parse(w)
                        .is_ok_and(|w| context.eval(&w))
                })
            })
            .map(|(_, m)| (m.location.clone(), m.command.clone()))
            .collect();
        let lookup = Lookup {
            core: &self.core,
            locale: &self.locale,
            keymap: self.core.config().keymap(),
        };
        menus::main_menu(&lookup, &|key| self.t(key), &contributions)
    }

    fn toggle_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.open_menu == Some(index) {
            self.open_menu = None;
        } else {
            self.menus = self.build_menus();
            self.open_menu = Some(index);
        }
        cx.notify();
    }

    /// The accessible description of the workbench.
    fn access_tree(&self) -> AccessTree {
        use accesskit::Role;
        let node = |id, role, label: String, children: Vec<u64>| AccessNode {
            id,
            role,
            label,
            children,
        };
        let mut regions = vec![node::MENU_BAR, node::TOOLBAR];
        if self.layout.left.is_some() {
            regions.push(node::PROJECT);
        }
        regions.push(node::EDITOR);
        if self.layout.bottom.is_some() {
            regions.push(node::BOTTOM);
        }
        regions.push(node::STATUS);
        let menu_ids: Vec<u64> = (0..self.menus.len() as u64)
            .map(|i| node::MENUS + i)
            .collect();
        let mut nodes = vec![
            node(node::WINDOW, Role::Window, self.t("app.name"), regions),
            node(node::MENU_BAR, Role::MenuBar, String::new(), menu_ids),
            node(node::TOOLBAR, Role::Toolbar, String::new(), vec![]),
            node(node::EDITOR, Role::Pane, self.t("editor.empty"), vec![]),
            node(
                node::STATUS,
                Role::Status,
                self.message.clone().unwrap_or_default(),
                vec![],
            ),
        ];
        if let Some(window) = self.layout.left {
            nodes.push(node(
                node::PROJECT,
                Role::Pane,
                self.t(window.title_key()),
                vec![],
            ));
        }
        if let Some(window) = self.layout.bottom {
            nodes.push(node(
                node::BOTTOM,
                Role::Pane,
                self.t(window.title_key()),
                vec![],
            ));
        }
        for (i, menu) in self.menus.iter().enumerate() {
            nodes.push(node(
                node::MENUS + i as u64,
                Role::MenuItem,
                menu.title.clone(),
                vec![],
            ));
        }
        AccessTree {
            nodes,
            focus: node::EDITOR,
        }
    }
}

/// Command titles, state and bindings for the menu.
struct Lookup<'a> {
    core: &'a CoreApi,
    locale: &'a str,
    keymap: kumiko_core::config::Keymap,
}

impl CommandLookup for Lookup<'_> {
    fn title(&self, command: &CommandId) -> Option<String> {
        let info = self
            .core
            .commands()
            .list()
            .into_iter()
            .find(|c| &c.id == command)?;
        let title = info.title.unwrap_or_else(|| command.to_string());
        Some(self.core.resources().label(&title, self.locale))
    }

    fn enabled(&self, command: &CommandId) -> bool {
        self.core.commands().is_enabled(command)
    }

    fn keys(&self, command: &CommandId) -> Option<String> {
        keys::binding_label(&self.keymap, command)
    }
}

fn palette(core: &CoreApi) -> Palette {
    let wanted = core
        .config()
        .get("workbench.theme", None)
        .and_then(|v| v.as_str().map(str::to_owned));
    let theme = wanted
        .as_deref()
        .and_then(|id| core.resources().theme(id))
        .or_else(|| core.resources().theme("kumiko.theme.darcula"));
    theme.map_or_else(Palette::fallback, |theme| Palette::from_theme(&theme))
}

/// An absolutely placed box for a region.
fn place(rect: Rect) -> gpui::Div {
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .overflow_hidden()
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let regions = self
            .layout
            .regions(f32::from(size.width), f32::from(size.height));
        let p = self.palette.clone();
        let tree = self.access_tree();
        if let Some(a11y) = &mut self.a11y {
            a11y.update(tree);
        }

        let mut root = div()
            .id("workbench")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .bg(rgba(p.window))
            .text_color(rgba(p.foreground))
            .font_family(UI_FONT)
            .text_size(px(UI_TEXT));

        root = root
            .child(self.menu_bar(regions.menu_bar, cx))
            .child(
                place(regions.toolbar)
                    .bg(rgba(p.toolbar))
                    .border_b_1()
                    .border_color(rgba(p.border))
                    .flex()
                    .items_center()
                    .px_2()
                    .child(self.folder_title()),
            )
            .child(self.stripe(Side::Left, regions.left_stripe, cx))
            .child(
                place(regions.right_stripe)
                    .bg(rgba(p.stripe))
                    .border_l_1()
                    .border_color(rgba(p.border)),
            )
            .child(self.stripe(Side::Bottom, regions.bottom_stripe, cx))
            .child(self.editor_area(regions.editor))
            .child(
                place(regions.status_bar)
                    .bg(rgba(p.status_bar))
                    .text_color(rgba(p.status_foreground))
                    .border_t_1()
                    .border_color(rgba(p.border))
                    .flex()
                    .items_center()
                    .px_2()
                    .text_size(px(12.0))
                    .child(self.message.clone().unwrap_or_default()),
            );
        if let (Some(rect), Some(window)) = (regions.left_panel, self.layout.left) {
            root = root.child(self.tool_window(window, rect, cx));
        }
        if let (Some(rect), Some(window)) = (regions.bottom_panel, self.layout.bottom) {
            root = root.child(self.tool_window(window, rect, cx));
        }
        if let Some(index) = self.open_menu {
            root = root.child(self.menu_popup(index, &regions, cx));
        }
        root
    }
}

impl Workbench {
    fn folder_title(&self) -> String {
        let folders = self.core.project().folders();
        match folders.first() {
            Some(folder) => folder.name.clone(),
            None => self.t("project.noFolder"),
        }
    }

    fn menu_bar(&self, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let items = self.menus.iter().enumerate().map(|(i, menu)| {
            let open = self.open_menu == Some(i);
            div()
                .id(("menu", i))
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .when(open, |d| d.bg(rgba(p.selection)))
                .hover(|d| d.bg(rgba(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_menu(i, cx);
                }))
                .child(menu.title.clone())
        });
        place(rect)
            .bg(rgba(p.menu_bar))
            .flex()
            .items_center()
            .px_1()
            .children(items)
            .into_any_element()
    }

    fn menu_popup(
        &self,
        index: usize,
        regions: &crate::layout::Regions,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &self.palette;
        // Menu titles are laid out by text width; place the popup under the menu bar at an
        // estimate, which is exact enough for a list that opens downwards.
        let x = 4.0
            + self.menus[..index]
                .iter()
                .map(|m| m.title.chars().count() as f32 * 7.0 + 16.0)
                .sum::<f32>();
        let menu = &self.menus[index];
        let mut list = div()
            .absolute()
            .left(px(x))
            .top(px(regions.menu_bar.h))
            .min_w(px(240.0))
            .py_1()
            .bg(rgba(p.popup))
            .border_1()
            .border_color(rgba(p.border))
            .shadow_md()
            .flex()
            .flex_col();
        if menu.items.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(rgba(p.muted))
                    .child(self.t("menu.empty")),
            );
        }
        for (i, item) in menu.items.iter().enumerate() {
            let Some(item) = item else {
                list = list.child(div().my_1().h(px(1.0)).bg(rgba(p.border)));
                continue;
            };
            let command = item.command.clone();
            let enabled = item.enabled;
            list = list.child(
                div()
                    .id(("menu-item", i))
                    .px_3()
                    .py_1()
                    .flex()
                    .justify_between()
                    .gap_6()
                    .when(!enabled, |d| d.text_color(rgba(p.muted)))
                    .when(enabled, |d| {
                        d.hover(|d| d.bg(rgba(p.selection))).on_click(cx.listener(
                            move |this, _: &ClickEvent, _window, cx| {
                                this.execute(command.clone(), cx);
                            },
                        ))
                    })
                    .child(item.title.clone())
                    .child(
                        div()
                            .text_color(rgba(p.muted))
                            .child(item.keys.clone().unwrap_or_default()),
                    ),
            );
        }
        // A click anywhere else closes the menu.
        div()
            .id("menu-overlay")
            .absolute()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.open_menu = None;
                    cx.notify();
                }),
            )
            .child(list.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }

    /// A stripe with the buttons of the tool windows on its edge.
    fn stripe(&self, side: Side, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let buttons = ToolWindow::ALL
            .into_iter()
            .filter(|w| w.side() == side)
            .map(|window| {
                let open = self.layout.is_open(window);
                let title = self.t(window.title_key());
                let command = CommandId::new(window.toggle_command()).ok();
                let button = div()
                    .id(window.toggle_command())
                    .when(open, |d| d.bg(rgba(p.stripe_active)))
                    .hover(|d| d.bg(rgba(p.hover)))
                    .text_size(px(12.0))
                    .when_some(command, |d, command| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.execute(command.clone(), cx);
                        }))
                    });
                match side {
                    // Classic UI writes side buttons vertically; GPUI does not rotate text,
                    // so the letters are stacked.
                    Side::Left => button
                        .w_full()
                        .py_2()
                        .flex()
                        .flex_col()
                        .items_center()
                        .children(
                            title
                                .chars()
                                .map(|c| div().h(px(13.0)).child(c.to_string())),
                        ),
                    Side::Bottom => button.h_full().px_2().flex().items_center().child(title),
                }
            });
        let stripe = place(rect).bg(rgba(p.stripe)).border_color(rgba(p.border));
        match side {
            Side::Left => stripe.border_r_1().flex().flex_col().children(buttons),
            Side::Bottom => stripe.border_t_1().flex().children(buttons),
        }
        .into_any_element()
    }

    fn tool_window(&self, window: ToolWindow, rect: Rect, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.palette;
        let hide = CommandId::new(window.toggle_command()).ok();
        let header = div()
            .h(px(24.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgba(p.panel_header))
            .border_b_1()
            .border_color(rgba(p.border))
            .child(self.t(window.title_key()))
            .child(
                div()
                    .id("hide")
                    .px_1()
                    .text_color(rgba(p.muted))
                    .hover(|d| d.bg(rgba(p.hover)))
                    .child("—")
                    .when_some(hide, |d, command| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.execute(command.clone(), cx);
                        }))
                    }),
            );
        let body = match window {
            ToolWindow::Project => {
                let folders = self.core.project().folders();
                if folders.is_empty() {
                    div()
                        .p_2()
                        .text_color(rgba(p.muted))
                        .child(self.t("project.noFolder"))
                } else {
                    div().py_1().children(
                        folders
                            .into_iter()
                            .map(|f| div().px_2().h(px(20.0)).flex().items_center().child(f.name)),
                    )
                }
            }
            ToolWindow::Problems | ToolWindow::Output => div()
                .p_2()
                .text_color(rgba(p.muted))
                .child(self.t("panel.empty")),
        };
        let panel = place(rect)
            .bg(rgba(p.panel))
            .flex()
            .flex_col()
            .child(header)
            .child(body);
        match window.side() {
            Side::Left => panel.border_r_1(),
            Side::Bottom => panel.border_t_1(),
        }
        .border_color(rgba(p.border))
        .into_any_element()
    }

    fn editor_area(&self, rect: Rect) -> AnyElement {
        let p = &self.palette;
        let hints: Vec<String> = {
            let keymap = self.core.config().keymap();
            let lookup = Lookup {
                core: &self.core,
                locale: &self.locale,
                keymap: keymap.clone(),
            };
            [
                "workbench.quickOpen",
                "workbench.showCommands",
                "workbench.toggleProject",
            ]
            .into_iter()
            .filter_map(|id| CommandId::new(id).ok())
            .filter_map(|id| {
                let title = lookup.title(&id)?;
                let keys = keys::binding_label(&keymap, &id)?;
                Some(format!("{title}   {keys}"))
            })
            .collect()
        };
        place(rect)
            .bg(rgba(p.editor))
            .text_color(rgba(p.muted))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_size(px(15.0)).child(self.t("editor.empty")))
            .children(hints.into_iter().map(|h| div().child(h)))
            .into_any_element()
    }
}
