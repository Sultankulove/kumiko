//! The Settings window (Ctrl+Alt+S, spec v1.1, section 6.1): the dialog of
//! [`crate::settings`] drawn over the workbench. Changes are written to the user or the
//! project settings file at once; the core reports them back as `config.changed`.

use gpui::{
    AnyElement, ClickEvent, Context, KeyDownEvent, MouseButton, Window, div, prelude::*, px, rgba,
};
use kumiko_core::config::{Layer, SettingKind};
use kumiko_core::protocol::methods::config::ConfigTarget;
use kumiko_core::resources::ResourceKind;
use kumiko_types::Uri;
use serde_json::Value;

use super::{FocusTarget, Workbench};
use crate::input::LineInput;
use crate::layout::Regions;
use crate::settings::{Change, SettingsDialog};

/// Height of a setting's row.
const ROW_HEIGHT: f32 = 56.0;

impl Workbench {
    pub(super) fn open_settings(&mut self) {
        self.open_menu = None;
        self.quick_open = None;
        let mut dialog = SettingsDialog::new(Vec::new(), &|_| None);
        self.fill_settings(&mut dialog);
        self.settings = Some(dialog);
        self.focus_request = Some(FocusTarget::Settings);
    }

    pub(super) fn close_settings(&mut self) {
        self.settings = None;
        self.focus_request = Some(if self.split.tabs().len() > 0 {
            FocusTarget::Editor
        } else {
            FocusTarget::Project
        });
    }

    /// Settings changed (`config.changed`, a theme or language switch): new values.
    pub(super) fn refresh_settings(&mut self) {
        if let Some(mut dialog) = self.settings.take() {
            self.fill_settings(&mut dialog);
            self.settings = Some(dialog);
        }
    }

    /// The current settings with their titles, and choices the schemas leave as text.
    fn fill_settings(&self, dialog: &mut SettingsDialog) {
        let title = |key: &str| {
            let id = format!("setting.{key}");
            let text = self.t(&id);
            (text != id).then_some(text)
        };
        dialog.replace(self.core.config().settings(), &title);
        let resources = self.core.resources();
        let packages = |kind| {
            resources
                .list(kind)
                .into_iter()
                .map(|package| {
                    let name = package.name.as_deref().unwrap_or(&package.id).to_owned();
                    (
                        Value::from(package.id),
                        resources.label(&name, &self.locale),
                    )
                })
                .collect::<Vec<_>>()
        };
        dialog.set_choices("workbench.theme", packages(ResourceKind::Theme));
        dialog.set_choices("workbench.iconTheme", packages(ResourceKind::Icons));
        dialog.set_choices(
            "workbench.locale",
            vec![
                (Value::Null, self.t("settings.systemLanguage")),
                (Value::from("en"), "English".to_owned()),
                (Value::from("ru"), "Русский".to_owned()),
            ],
        );
    }

    fn section_title(&self, section: &str) -> String {
        let id = format!("settings.section.{section}");
        let text = self.t(&id);
        if text == id {
            crate::settings::humanize(section)
        } else {
            text
        }
    }

    fn apply_settings_change(&mut self, change: Change, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.settings else {
            return;
        };
        let target = dialog.target;
        match change {
            Change::Set(key, value) => {
                if let Err(error) = self.core.config().update(&key, value, target) {
                    dialog.error = Some(error.to_string());
                } else {
                    dialog.error = None;
                    self.refresh_settings();
                }
            }
            Change::OpenFile => {
                let Some(path) = self.core.config().file(target) else {
                    let message = self.t("settings.noWorkspace");
                    if let Some(dialog) = &mut self.settings {
                        dialog.error = Some(message);
                    }
                    return;
                };
                if !path.exists() {
                    let created = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&path, ""));
                    if let Err(error) = created {
                        dialog.error = Some(error.to_string());
                        return;
                    }
                }
                match Uri::from_file_path(&path) {
                    Ok(uri) => {
                        self.close_settings();
                        self.open_file(uri, cx);
                    }
                    Err(error) => dialog.error = Some(error.to_string()),
                }
            }
        }
    }

    pub(super) fn settings_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.settings else {
            return;
        };
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        let shortcut = modifiers.control || modifiers.platform || modifiers.alt;
        let editing = dialog.editing.is_some();
        let mut change = None;
        let mut searched = false;
        match stroke.key.as_str() {
            "escape" => {
                if !dialog.cancel_edit() {
                    self.close_settings();
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            "enter" => change = dialog.activate(),
            "tab" => dialog.toggle_target(),
            "down" => dialog.move_selection(1),
            "up" => dialog.move_selection(-1),
            "pagedown" => dialog.move_selection(8),
            "pageup" => dialog.move_selection(-8),
            "left" | "right" if !editing && is_choice(dialog) => {
                change = dialog.step(if stroke.key == "left" { -1 } else { 1 });
            }
            // Delete with nothing after the caret of the search: back to the layer below.
            "delete" if !editing && dialog.search.split().1.is_empty() => {
                change = dialog.reset();
            }
            key => {
                let input: &mut LineInput = match &mut dialog.editing {
                    Some(input) => input,
                    None => &mut dialog.search,
                };
                match key {
                    "left" => input.left(),
                    "right" => input.right(),
                    "home" => input.home(),
                    "end" => input.end(),
                    "backspace" => {
                        input.backspace();
                        searched = !editing;
                    }
                    "delete" => {
                        input.delete();
                        searched = !editing;
                    }
                    "a" if modifiers.control || modifiers.platform => input.select_all(),
                    _ => match stroke.key_char.as_deref() {
                        Some(text) if !shortcut => {
                            input.insert(text);
                            searched = !editing;
                        }
                        // Other shortcuts (Ctrl+Alt+S again, …) go on to the keymap.
                        _ => return,
                    },
                }
            }
        }
        if searched {
            dialog.filter();
        }
        cx.stop_propagation();
        if let Some(change) = change {
            self.apply_settings_change(change, cx);
        }
        cx.notify();
    }

    pub(super) fn settings_popup(
        &self,
        regions: &Regions,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.settings.as_ref()?;
        let p = &self.palette;
        let window_w = regions.menu_bar.w;
        let window_h = regions.status_bar.y + regions.status_bar.h;
        let width = (window_w * 0.8).clamp(560.0, 960.0).min(window_w - 16.0);
        let height = (window_h * 0.8).clamp(360.0, 680.0).min(window_h - 16.0);
        let x = ((window_w - width) / 2.0).max(0.0);
        let y = ((window_h - height) / 2.0).max(0.0);

        let caret = || div().w(px(1.0)).h(px(16.0)).bg(rgba(p.foreground));
        let field = |input: &LineInput| {
            let (before, after) = input.split();
            div().flex().items_center().whitespace_nowrap().map(|d| {
                if input.is_all_selected() {
                    d.child(div().bg(rgba(p.selection)).child(before.to_owned()))
                        .child(caret())
                } else {
                    d.child(before.to_owned())
                        .child(caret())
                        .child(after.to_owned())
                }
            })
        };

        // Header: title, search, where changes go.
        let search = div()
            .flex_1()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .bg(rgba(p.input))
            .border_1()
            .border_color(rgba(if dialog.editing.is_none() {
                p.focus_border
            } else {
                p.border
            }))
            .child(field(&dialog.search))
            .when(dialog.search.text().is_empty(), |d| {
                d.child(
                    div()
                        .pl_2()
                        .text_color(rgba(p.muted))
                        .child(self.t("settings.search")),
                )
            });
        let target = |which: ConfigTarget, label: &str| {
            let active = dialog.target == which;
            div()
                .id(match which {
                    ConfigTarget::User => "settings-user",
                    ConfigTarget::Workspace => "settings-workspace",
                })
                .px_2()
                .py_0p5()
                .when(active, |d| d.bg(rgba(p.selection)))
                .hover(|d| d.bg(rgba(p.hover)))
                .child(self.t(label))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if let Some(dialog) = &mut this.settings
                        && dialog.target != which
                    {
                        dialog.toggle_target();
                    }
                    cx.notify();
                }))
        };
        let header = div()
            .h(px(44.0))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgba(p.border))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(self.t("settings.title")),
            )
            .child(search)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_1()
                    .border_color(rgba(p.border))
                    .child(target(ConfigTarget::User, "settings.target.user"))
                    .child(target(ConfigTarget::Workspace, "settings.target.workspace")),
            );

        // Sections on the left.
        let section_row = |id: usize, title: String, section: Option<String>| {
            let active = dialog.section == section;
            div()
                .id(("settings-section", id))
                .h(px(26.0))
                .px_3()
                .flex()
                .items_center()
                .when(active, |d| d.bg(rgba(p.selection)))
                .hover(|d| d.bg(rgba(p.hover)))
                .child(title)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if let Some(dialog) = &mut this.settings {
                        dialog.set_section(section.clone());
                    }
                    cx.notify();
                }))
        };
        let mut sections = vec![section_row(0, self.t("settings.all"), None)];
        for (i, section) in dialog.sections().into_iter().enumerate() {
            sections.push(section_row(
                i + 1,
                self.section_title(&section),
                Some(section),
            ));
        }
        let left = div()
            .w(px(190.0))
            .flex_none()
            .py_1()
            .border_r_1()
            .border_color(rgba(p.border))
            .bg(rgba(p.panel))
            .flex()
            .flex_col()
            .children(sections);

        // Settings on the right: a window of rows around the selection.
        let visible = ((height - 44.0 - 28.0) / ROW_HEIGHT).floor().max(1.0) as usize;
        let first = dialog.selected.saturating_sub(visible - 1);
        let layer_label = |layer: Layer| {
            self.t(match layer {
                Layer::Default => "settings.layer.default",
                Layer::Kit => "settings.layer.kit",
                Layer::User => "settings.layer.user",
                Layer::Workspace => "settings.layer.workspace",
            })
        };
        let rows = dialog
            .shown()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, row)| {
                let selected = i == dialog.selected;
                let control: AnyElement = match &row.kind {
                    SettingKind::Boolean => {
                        let on = row.value.as_ref().and_then(Value::as_bool).unwrap_or(false);
                        div()
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgba(p.border))
                            .when(on, |d| d.bg(rgba(p.accent)).child("✓"))
                            .into_any_element()
                    }
                    SettingKind::Choice(_) => div()
                        .flex()
                        .gap_2()
                        .child(div().text_color(rgba(p.muted)).child("‹"))
                        .child(row.display())
                        .child(div().text_color(rgba(p.muted)).child("›"))
                        .into_any_element(),
                    SettingKind::Structured => div()
                        .text_color(rgba(p.accent))
                        .child(self.t("settings.editFile"))
                        .into_any_element(),
                    SettingKind::Number { .. } | SettingKind::Text { .. } => {
                        match (&dialog.editing, selected) {
                            (Some(input), true) => div()
                                .min_w(px(160.0))
                                .h(px(24.0))
                                .px_2()
                                .bg(rgba(p.input))
                                .border_1()
                                .border_color(rgba(p.focus_border))
                                .child(field(input))
                                .into_any_element(),
                            _ => div()
                                .max_w(px(320.0))
                                .truncate()
                                .child(row.display())
                                .into_any_element(),
                        }
                    }
                };
                let resettable = match dialog.target {
                    ConfigTarget::User => row.layer == Layer::User,
                    ConfigTarget::Workspace => row.layer == Layer::Workspace,
                };
                div()
                    .id(("settings-row", i))
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(selected, |d| d.bg(rgba(p.selection_inactive)))
                    .hover(|d| d.bg(rgba(p.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        let change = this.settings.as_mut().and_then(|dialog| {
                            let again = dialog.selected == i;
                            dialog.select(i);
                            // A second click on the row (or the first on a switch) changes it.
                            let switch = matches!(
                                dialog.current().map(|r| &r.kind),
                                Some(SettingKind::Boolean | SettingKind::Choice(_))
                            );
                            (again || switch).then(|| dialog.activate()).flatten()
                        });
                        if let Some(change) = change {
                            this.apply_settings_change(change, cx);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .items_baseline()
                                    .child(row.title.clone())
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(rgba(p.muted))
                                            .child(row.key.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgba(p.muted))
                                    .truncate()
                                    .child(row.description.clone()),
                            ),
                    )
                    .child(control)
                    .child(
                        div()
                            .w(px(110.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(rgba(p.muted))
                            .child(layer_label(row.layer)),
                    )
                    .child(
                        div()
                            .id(("settings-reset", i))
                            .w(px(18.0))
                            .flex_none()
                            .when(resettable, |d| {
                                d.child("↺")
                                    .text_color(rgba(p.accent))
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _window, cx| {
                                            cx.stop_propagation();
                                            let change = this.settings.as_mut().and_then(|d| {
                                                d.select(i);
                                                d.reset()
                                            });
                                            if let Some(change) = change {
                                                this.apply_settings_change(change, cx);
                                            }
                                            cx.notify();
                                        },
                                    ))
                            }),
                    )
            })
            .collect::<Vec<_>>();
        let list = div()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(dialog.shown_len() == 0, |d| {
                d.child(
                    div()
                        .p_3()
                        .text_color(rgba(p.muted))
                        .child(self.t("settings.none")),
                )
            })
            .children(rows);

        let footer = div()
            .h(px(28.0))
            .px_3()
            .flex()
            .items_center()
            .border_t_1()
            .border_color(rgba(p.border))
            .text_size(px(12.0))
            .map(|d| match &dialog.error {
                Some(error) => d.text_color(rgba(p.error)).child(error.clone()),
                None => d.text_color(rgba(p.muted)).child(self.t("settings.hint")),
            });

        let window = div()
            .id("settings")
            .track_focus(&self.settings_focus)
            .on_key_down(cx.listener(Self::settings_key))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .h(px(height))
            .bg(rgba(p.popup))
            .text_color(rgba(p.foreground))
            .border_1()
            .border_color(rgba(p.focus_border))
            .shadow_lg()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h(px(0.0)).flex().child(left).child(list))
            .child(footer)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // Modal: a click outside closes it.
        Some(
            div()
                .id("settings-overlay")
                .absolute()
                .size_full()
                .bg(gpui::rgba(0x0000_0040))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.close_settings();
                        cx.notify();
                    }),
                )
                .child(window)
                .into_any_element(),
        )
    }
}

fn is_choice(dialog: &SettingsDialog) -> bool {
    matches!(
        dialog.current().map(|row| &row.kind),
        Some(SettingKind::Choice(_))
    )
}
