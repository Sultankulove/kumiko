//! Language commands of the workbench, at the active editor's caret: File Structure (`@` in
//! Quick Open, Ctrl+F12), Find Usages (Alt+F7), Rename (Shift+F6), Reformat Code
//! (Ctrl+Alt+L). The results come from the core's Language service.

use gpui::Context;
use kumiko_types::{Position, Uri};

use super::{Workbench, file_name};
use crate::quick_open::{Action, Item, Mode, QuickOpen};

/// Results Find Usages lists at most.
const MAX_USAGES: usize = 500;

/// Case-insensitive match of every typed char in order.
fn matches(label: &str, query: &str) -> bool {
    let mut chars = label.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.any(|c| c == q))
}

impl Workbench {
    /// The active editor's file and caret.
    fn caret_target(&self, cx: &Context<Self>) -> Option<(Uri, Position, String)> {
        let uri = self.split.tabs().active()?.uri.clone();
        let status = (self.active_editor()?.status)(cx);
        Some((uri, status.caret, status.word))
    }

    /// Fetches the active file's symbols when `@` mode needs them.
    pub(super) fn ensure_symbols(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .quick_open
            .as_ref()
            .is_some_and(|q| q.mode() == Mode::Symbols);
        let Some(uri) = self.split.tabs().active().map(|t| t.uri.clone()) else {
            return;
        };
        if !wanted || self.symbols.as_ref().is_some_and(|(u, _)| *u == uri) {
            return;
        }
        self.symbols = Some((uri.clone(), Vec::new()));
        let core = self.core.clone();
        let fetched = uri.clone();
        let task = self
            .runtime
            .spawn(async move { core.language().symbols(&fetched).await });
        cx.spawn(async move |this, cx| {
            let Ok(symbols) = task.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                workbench.symbols = Some((uri, symbols));
                workbench.refresh_quick();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn symbol_items(&self, query: &str) -> Vec<Item> {
        let Some((uri, symbols)) = &self.symbols else {
            return vec![self.note("quickOpen.symbolsUnavailable")];
        };
        symbols
            .iter()
            .filter(|s| matches(&s.name, query))
            .map(|s| Item {
                label: format!("{}{}", "  ".repeat(s.depth), s.name),
                highlights: Vec::new(),
                detail: s.detail.clone().unwrap_or_else(|| s.kind.to_owned()),
                keys: None,
                action: Action::OpenAt(uri.clone(), s.selection.start),
            })
            .collect()
    }

    pub(super) fn usage_items(&self, query: &str) -> Vec<Item> {
        self.usages
            .iter()
            .filter(|item| matches(&item.label, query) || matches(&item.detail, query))
            .cloned()
            .collect()
    }

    pub(super) fn rename_items(&self, query: &str) -> Vec<Item> {
        let label = if query.is_empty() {
            self.t("rename.prompt")
        } else {
            self.t("rename.to").replace("{name}", query)
        };
        vec![Item {
            label,
            highlights: Vec::new(),
            detail: String::new(),
            keys: Some("Enter".into()),
            action: if query.is_empty() {
                Action::None
            } else {
                Action::Rename
            },
        }]
    }

    /// Alt+F7: usages of the symbol at the caret, as a list to pick from; one usage opens.
    pub(super) fn find_usages(&mut self, cx: &mut Context<Self>) {
        let Some((uri, caret, word)) = self.caret_target(cx) else {
            return;
        };
        let core = self.core.clone();
        let task = self.runtime.spawn(async move {
            let found = core.language().references(&uri, caret).await;
            // Each usage with its line, from the open document or the file.
            let mut rows = Vec::new();
            for location in found.into_iter().take(MAX_USAGES) {
                let line = match core.documents().find(&location.uri) {
                    Some(id) => core.documents().snapshot(id).ok().and_then(|s| {
                        let n = location.range.start.line as usize;
                        (n < s.text.len_lines()).then(|| s.text.line(n).to_string())
                    }),
                    None => location.uri.to_file_path().and_then(|path| {
                        std::fs::read_to_string(path).ok().and_then(|text| {
                            text.lines()
                                .nth(location.range.start.line as usize)
                                .map(str::to_owned)
                        })
                    }),
                };
                rows.push((location, line.unwrap_or_default()));
            }
            rows
        });
        cx.spawn(async move |this, cx| {
            let Ok(rows) = task.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                if rows.is_empty() {
                    workbench.message = Some(workbench.t("usages.none").replace("{name}", &word));
                    cx.notify();
                    return;
                }
                workbench.usages = rows
                    .into_iter()
                    .map(|(location, line)| Item {
                        label: line.trim().to_owned(),
                        highlights: Vec::new(),
                        detail: format!(
                            "{}:{}",
                            file_name(&location.uri),
                            location.range.start.line + 1
                        ),
                        keys: None,
                        action: Action::OpenAt(location.uri, location.range.start),
                    })
                    .collect();
                workbench.open_menu = None;
                workbench.quick_open = Some(QuickOpen::picker(Mode::Usages));
                workbench.refresh_quick();
                workbench.focus_request = Some(super::FocusTarget::Quick);
                cx.notify();
            });
        })
        .detach();
    }

    /// Shift+F6: asks for the new name, starting from the current one.
    pub(super) fn open_rename(&mut self, cx: &mut Context<Self>) {
        let Some((uri, caret, word)) = self.caret_target(cx) else {
            return;
        };
        self.language_target = Some((uri, caret));
        self.open_menu = None;
        self.quick_open = Some(QuickOpen::prompt(Mode::Rename, &word));
        self.refresh_quick();
        self.focus_request = Some(super::FocusTarget::Quick);
    }

    pub(super) fn rename(&mut self, new_name: String, cx: &mut Context<Self>) {
        let Some((uri, caret)) = self.language_target.take() else {
            return;
        };
        let core = self.core.clone();
        let task = self
            .runtime
            .spawn(async move { core.language().rename(&uri, caret, &new_name).await });
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |workbench, cx| {
                workbench.message = Some(match result {
                    Ok(files) => workbench
                        .t("rename.done")
                        .replace("{files}", &files.to_string()),
                    Err(error) => error,
                });
                workbench.focus_request = Some(super::FocusTarget::Editor);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ctrl+Alt+L: the language server formats the active file.
    pub(super) fn reformat(&mut self, cx: &mut Context<Self>) {
        let Some((uri, _, _)) = self.caret_target(cx) else {
            return;
        };
        let config = self.core.config();
        let tab_size = config
            .get("editor.tabSize", None)
            .and_then(|v| v.as_u64())
            .unwrap_or(4) as u32;
        let insert_spaces = config
            .get("editor.insertSpaces", None)
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let core = self.core.clone();
        let task = self
            .runtime
            .spawn(async move { core.language().format(&uri, tab_size, insert_spaces).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = task.await {
                let _ = this.update(cx, |workbench, cx| {
                    workbench.message = Some(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
