//! Language features in the editor, through the core's Language service (the editor never
//! sees LSP): diagnostics under the text, completion (Ctrl+Space, after `.`), intentions
//! (Alt+Enter), Go to Declaration (Ctrl+B), quick documentation (Ctrl+Q).

use std::ops::Range as StdRange;

use gpui::{AnyElement, Bounds, ClickEvent, Context, Pixels, div, point, prelude::*, px};
use kumiko_core::language::{CompletionItem, Severity};
use kumiko_types::{Position, TextEdit};

use super::EditorView;
use crate::popup::{Actions, Completion, Popup};
use crate::selection::{Assoc, Change, Range, Selection, map_position};
use crate::text::{char_of, position_of};

/// Rows a popup shows before it scrolls.
const POPUP_ROWS: usize = 12;

/// A diagnostic of the document, in chars of the current text.
pub(super) struct Mark {
    pub range: StdRange<usize>,
    pub severity: Severity,
    pub message: String,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

impl EditorView {
    fn t(&self, key: &str) -> String {
        self.core.resources().localize(key, &self.core.locale())
    }

    // ---- diagnostics ----------------------------------------------------------------------

    /// Reads the merged diagnostics of the document again.
    pub(super) fn refresh_diagnostics(&mut self) {
        let rope = &self.rope;
        self.marks = self
            .core
            .language()
            .diagnostics(&self.uri)
            .into_iter()
            .map(|d| {
                let start = char_of(rope, d.range.start);
                let mut end = char_of(rope, d.range.end).max(start);
                // An empty range still gets a visible mark: the char after it.
                if end == start && start < rope.len_chars() {
                    end = start + 1;
                }
                Mark {
                    range: start..end,
                    severity: d.severity,
                    message: d.message,
                }
            })
            .collect();
    }

    /// Edits move the marks until the server reports again.
    pub(super) fn map_marks(&mut self, changes: &[Change]) {
        for mark in &mut self.marks {
            let start = map_position(mark.range.start, changes, Assoc::After);
            let end = map_position(mark.range.end, changes, Assoc::Before).max(start);
            mark.range = start..end;
        }
        self.marks.retain(|m| !m.range.is_empty());
    }

    /// The message of a diagnostic under the primary caret, for the status bar.
    pub fn diagnostic_at_caret(&self) -> Option<&str> {
        let head = self.selection.primary().head;
        self.marks
            .iter()
            .filter(|m| m.range.start <= head && head <= m.range.end)
            .min_by_key(|m| m.severity)
            .map(|m| m.message.as_str())
    }

    // ---- commands -------------------------------------------------------------------------

    /// Language commands; `false` for others.
    pub(super) fn language_command(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        match command {
            "editor.complete" => self.complete(cx),
            "editor.showIntentions" => self.show_intentions(cx),
            "editor.gotoDefinition" => self.goto_definition(cx),
            "editor.quickDocumentation" => self.quick_documentation(cx),
            "editor.parameterInfo" => self.parameter_info(cx),
            _ => return false,
        }
        true
    }

    /// Keys while a popup is open: it takes Up/Down, Enter/Tab and Escape.
    pub(super) fn popup_command(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        let Some(popup) = &mut self.popup else {
            return false;
        };
        // Parameter info stays while typing the call; only Escape closes it.
        if matches!(popup, Popup::Signature { .. }) {
            if command == "editor.escape" {
                self.popup = None;
                cx.notify();
                return true;
            }
            return false;
        }
        let delta = match command {
            "editor.cursorUp" => -1,
            "editor.cursorDown" => 1,
            "editor.cursorPageUp" => -(POPUP_ROWS as isize),
            "editor.cursorPageDown" => POPUP_ROWS as isize,
            "editor.newline" | "editor.indent" => 0,
            "editor.escape" => {
                self.popup = None;
                cx.notify();
                return true;
            }
            // Anything else closes an information popup and goes on as usual.
            _ => {
                if matches!(popup, Popup::Info(_)) {
                    self.popup = None;
                }
                return false;
            }
        };
        match popup {
            Popup::Completion(completion) if delta != 0 => completion.move_by(delta),
            Popup::Actions(actions) if delta != 0 => actions.move_by(delta),
            Popup::Completion(_) => self.accept_completion(cx),
            Popup::Actions(_) => self.run_selected_action(cx),
            Popup::Info(_) | Popup::Signature { .. } => {
                self.popup = None;
                return false;
            }
        }
        cx.notify();
        true
    }

    /// After typed text: the completion list narrows, or opens after `.`.
    pub(super) fn after_typing(&mut self, text: &str, cx: &mut Context<Self>) {
        if matches!(self.popup, Some(Popup::Info(_))) {
            self.popup = None;
        }
        let signature = matches!(self.popup, Some(Popup::Signature { .. }));
        if let Some(Popup::Completion(_)) = &self.popup {
            self.refilter();
        } else if text == "." && self.selection.len() == 1 {
            self.complete(cx);
        } else if text == ")" && signature {
            self.popup = None;
        } else if text == "(" || (text == "," && signature) {
            self.parameter_info(cx);
        }
    }

    /// The completion list follows the word under the caret; leaving it closes the list.
    pub(super) fn refilter(&mut self) {
        let head = self.selection.primary().head;
        let Some(Popup::Completion(completion)) = &mut self.popup else {
            return;
        };
        let typed: Option<String> =
            (head >= completion.start).then(|| self.rope.slice(completion.start..head).to_string());
        match typed {
            Some(typed) if typed.chars().all(is_word) => {
                completion.filter(&typed);
                if completion.is_empty() {
                    self.popup = None;
                }
            }
            _ => self.popup = None,
        }
    }

    fn word_start(&self, head: usize) -> usize {
        let mut start = head;
        while start > 0 && is_word(self.rope.char(start - 1)) {
            start -= 1;
        }
        start
    }

    fn caret(&self) -> Position {
        position_of(&self.rope, self.selection.primary().head)
    }

    /// The primary caret as a document position.
    pub fn caret_location(&self) -> Position {
        self.caret()
    }

    /// The identifier the primary caret is in or right after.
    pub fn word_at_caret(&self) -> String {
        let head = self.selection.primary().head;
        let mut end = head;
        while end < self.rope.len_chars() && is_word(self.rope.char(end)) {
            end += 1;
        }
        self.rope.slice(self.word_start(head)..end).to_string()
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        let head = self.selection.primary().head;
        let start = self.word_start(head);
        let (core, uri, position) = (self.core.clone(), self.uri.clone(), self.caret());
        let task = self
            .runtime
            .spawn(async move { core.language().completion(&uri, position).await });
        let version = self.version;
        cx.spawn(async move |this, cx| {
            let Ok(items) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                // The caret moved to another word meanwhile: the answer is stale.
                let head = view.selection.primary().head;
                if head < start || (view.version != version && view.word_start(head) != start) {
                    return;
                }
                if items.is_empty() {
                    view.popup = Some(Popup::Info(view.t("editor.noSuggestions")));
                } else {
                    let typed = view.rope.slice(start..head).to_string();
                    let completion = Completion::new(items, start, &typed);
                    view.popup = (!completion.is_empty()).then_some(Popup::Completion(completion));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Replaces the word with the chosen item; edits the item needs elsewhere (imports) come
    /// with `completionItem/resolve` and are applied in the same transaction.
    fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(Popup::Completion(completion)) = self.popup.take() else {
            return;
        };
        let Some(item) = completion.current().cloned() else {
            return;
        };
        let start = completion.start;
        let (core, uri) = (self.core.clone(), self.uri.clone());
        let task = self
            .runtime
            .spawn(async move { core.language().resolve_completion(&uri, item).await });
        cx.spawn(async move |this, cx| {
            let Ok(item) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| view.insert_completion(start, &item, cx));
        })
        .detach();
        cx.notify();
    }

    fn insert_completion(&mut self, start: usize, item: &CompletionItem, cx: &mut Context<Self>) {
        let head = self.selection.primary().head;
        // The server's range started the word; it ends at the caret now (typed further).
        let from = item
            .edit
            .as_ref()
            .map_or(start, |e| char_of(&self.rope, e.range.start))
            .min(head);
        let text = item
            .edit
            .as_ref()
            .map_or(item.insert_text.as_str(), |e| e.new_text.as_str());
        let mut edits: Vec<TextEdit> = vec![TextEdit {
            range: kumiko_types::Range::new(position_of(&self.rope, from), self.caret()),
            new_text: text.to_owned(),
        }];
        edits.extend(
            item.additional_edits
                .iter()
                .filter(|e| char_of(&self.rope, e.range.end) <= from)
                .cloned(),
        );
        let added_before: isize = item
            .additional_edits
            .iter()
            .filter(|e| char_of(&self.rope, e.range.end) <= from)
            .map(|e| {
                e.new_text.chars().count() as isize
                    - (char_of(&self.rope, e.range.end) - char_of(&self.rope, e.range.start))
                        as isize
            })
            .sum();
        match self.core.documents().apply(self.doc, self.version, edits) {
            Ok(_) => {
                if let Ok(snapshot) = self.core.documents().snapshot(self.doc) {
                    let caret = (from as isize + added_before) as usize + text.chars().count();
                    self.load(snapshot);
                    self.selection = Selection::point(caret.min(self.rope.len_chars()));
                    self.apply_folds();
                    self.refresh_matches();
                }
            }
            Err(error) => self.message = Some(error.to_string()),
        }
        self.reveal();
        cx.notify();
    }

    fn show_intentions(&mut self, cx: &mut Context<Self>) {
        let primary = self.selection.primary();
        let range = kumiko_types::Range::new(
            position_of(&self.rope, primary.start()),
            position_of(&self.rope, primary.end()),
        );
        let (core, uri) = (self.core.clone(), self.uri.clone());
        let task = self
            .runtime
            .spawn(async move { core.language().code_actions(&uri, range).await });
        cx.spawn(async move |this, cx| {
            let Ok(actions) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.popup = Some(if actions.is_empty() {
                    Popup::Info(view.t("editor.noIntentions"))
                } else {
                    Popup::Actions(Actions {
                        actions,
                        selected: 0,
                    })
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn run_selected_action(&mut self, cx: &mut Context<Self>) {
        let Some(Popup::Actions(actions)) = self.popup.take() else {
            return;
        };
        let Some(action) = actions.actions.get(actions.selected).cloned() else {
            return;
        };
        let core = self.core.clone();
        let task = self
            .runtime
            .spawn(async move { core.language().apply_code_action(&action).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = task.await {
                let _ = this.update(cx, |view, cx| {
                    view.message = Some(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Ctrl+B: the first declaration; another file opens through the workbench's command.
    fn goto_definition(&mut self, cx: &mut Context<Self>) {
        let (core, uri, position) = (self.core.clone(), self.uri.clone(), self.caret());
        let task = self.runtime.spawn(async move {
            let found = core.language().definition(&uri, position).await;
            let target = found.into_iter().next()?;
            if target.uri != uri {
                let args = vec![
                    serde_json::Value::from(target.uri.as_str()),
                    serde_json::Value::from(target.range.start.line),
                    serde_json::Value::from(target.range.start.character),
                ];
                if let Ok(id) = kumiko_types::CommandId::new("workbench.openFile") {
                    let _ = core.commands().execute(&id, args).await;
                }
            }
            Some(target)
        });
        cx.spawn(async move |this, cx| {
            let target = task.await.ok().flatten();
            let _ = this.update(cx, |view, cx| match target {
                Some(target) if target.uri == view.uri => {
                    let at = char_of(&view.rope, target.range.start);
                    view.selection = Selection::point(at);
                    view.unfold_around_carets();
                    view.center_caret();
                    cx.notify();
                }
                Some(_) => {}
                None => {
                    view.popup = Some(Popup::Info(view.t("editor.noDeclaration")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn quick_documentation(&mut self, cx: &mut Context<Self>) {
        let (core, uri, position) = (self.core.clone(), self.uri.clone(), self.caret());
        let task = self
            .runtime
            .spawn(async move { core.language().hover(&uri, position).await });
        cx.spawn(async move |this, cx| {
            let Ok(hover) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                let text = hover
                    .map(|h| h.contents)
                    .or_else(|| view.diagnostic_at_caret().map(str::to_owned))
                    .unwrap_or_else(|| view.t("editor.noDocumentation"));
                view.popup = Some(Popup::Info(text));
                cx.notify();
            });
        })
        .detach();
    }

    /// Ctrl+P (and after `(`): the signature of the call around the caret.
    fn parameter_info(&mut self, cx: &mut Context<Self>) {
        let (core, uri, position) = (self.core.clone(), self.uri.clone(), self.caret());
        let task = self
            .runtime
            .spawn(async move { core.language().signature_help(&uri, position).await });
        cx.spawn(async move |this, cx| {
            let Ok(help) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                match help.and_then(|h| {
                    h.signatures
                        .first()
                        .cloned()
                        .map(|l| (l, h.active_parameter))
                }) {
                    Some((label, active)) => view.popup = Some(Popup::Signature { label, active }),
                    None if matches!(view.popup, Some(Popup::Signature { .. })) => {
                        view.popup = None
                    }
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves the caret to `position` and scrolls it into the middle (Go to, Problems).
    pub fn goto_position(&mut self, position: Position, cx: &mut Context<Self>) {
        let at = char_of(&self.rope, position);
        self.selection = Selection::single(Range::point(at));
        self.unfold_around_carets();
        self.center_caret();
        cx.notify();
    }

    // ---- the popup ------------------------------------------------------------------------

    fn caret_bounds(&self) -> Option<Bounds<Pixels>> {
        let head = self.selection.primary().head;
        let layout = self.layout.as_ref()?;
        let (row, _) = self.map.locate(&self.rope, head);
        let row_layout = layout.rows.get(row.checked_sub(layout.first_row)?)?;
        let top = layout.bounds.top() + px(row as f32 * self.style.line_height) - self.scroll.y;
        let x = layout.text_left - self.scroll.x + row_layout.x_for(head);
        Some(Bounds::new(
            point(x, top),
            gpui::size(px(2.0), px(self.style.line_height)),
        ))
    }

    /// The open popup, below the caret, in front of everything.
    pub(super) fn render_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let caret = self.caret_bounds()?;
        let style = &self.style;
        let row = |id: (&'static str, usize), selected: bool| {
            div()
                .id(id)
                .h(px(style.line_height + 2.0))
                .px_2()
                .flex()
                .items_center()
                .gap_3()
                .whitespace_nowrap()
                .when(selected, |d| d.bg(style.popup_selection))
        };
        let content: AnyElement = match popup {
            Popup::Completion(completion) => {
                let first = completion.selected.saturating_sub(POPUP_ROWS - 1);
                let rows = completion
                    .shown
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(POPUP_ROWS)
                    .map(|(n, &i)| {
                        let item = &completion.items[i];
                        row(("completion", n), n == completion.selected)
                            .on_click(cx.listener(move |view, _: &ClickEvent, _window, cx| {
                                if let Some(Popup::Completion(c)) = &mut view.popup {
                                    c.selected = n;
                                }
                                view.accept_completion(cx);
                            }))
                            .child(div().flex_1().child(item.label.clone()))
                            .children(
                                item.detail
                                    .clone()
                                    .map(|d| div().text_color(style.gutter_foreground).child(d)),
                            )
                    });
                div().flex().flex_col().children(rows).into_any_element()
            }
            Popup::Actions(actions) => {
                // A window of the list around the selection, like completion.
                let first = actions.selected.saturating_sub(POPUP_ROWS - 1);
                let rows = actions
                    .actions
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(POPUP_ROWS)
                    .map(|(n, action)| {
                        row(("intention", n), n == actions.selected)
                            .on_click(cx.listener(move |view, _: &ClickEvent, _window, cx| {
                                if let Some(Popup::Actions(a)) = &mut view.popup {
                                    a.selected = n;
                                }
                                view.run_selected_action(cx);
                            }))
                            .child(div().text_color(style.warning).child("💡"))
                            .child(action.title.clone())
                    });
                div().flex().flex_col().children(rows).into_any_element()
            }
            Popup::Signature { label, active } => {
                let (before, current, after) = match active {
                    Some(range) if range.end <= label.len() => (
                        &label[..range.start],
                        &label[range.clone()],
                        &label[range.end..],
                    ),
                    _ => (label.as_str(), "", ""),
                };
                div()
                    .px_2()
                    .py_0p5()
                    .flex()
                    .whitespace_nowrap()
                    .child(before.to_owned())
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(current.to_owned()),
                    )
                    .child(after.to_owned())
                    .into_any_element()
            }
            Popup::Info(text) => div()
                .max_w(px(600.0))
                .p_2()
                .children(text.lines().map(|line| div().child(line.to_owned())))
                .into_any_element(),
        };
        // Parameter info sits above the line, as in IntelliJ; lists go below.
        let above = matches!(popup, Popup::Signature { .. });
        let (corner, at) = if above {
            (gpui::Corner::BottomLeft, point(caret.left(), caret.top()))
        } else {
            (gpui::Corner::TopLeft, point(caret.left(), caret.bottom()))
        };
        Some(
            gpui::deferred(
                gpui::anchored()
                    .anchor(corner)
                    .position(at)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .id("editor-popup")
                            .min_w(px(220.0))
                            .max_h(px((style.line_height + 2.0) * POPUP_ROWS as f32 + 4.0))
                            .overflow_hidden()
                            .py_0p5()
                            .bg(style.popup_background)
                            .border_1()
                            .border_color(style.line_highlight)
                            .text_color(style.foreground)
                            .font_family(style.font_family.clone())
                            .text_size(px(style.font_size))
                            .shadow_md()
                            .child(content),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
