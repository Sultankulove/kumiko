//! Find in Files (Ctrl+Shift+F, MVP criterion 10): a Quick Open popup whose input is the query;
//! results stream in from the core while typing (each keystroke starts a new search after a
//! short pause). Alt+C, Alt+W, Alt+X toggle case, whole word and regular expression, as in
//! the editor's find bar.

use std::time::Duration;

use gpui::Context;
use kumiko_core::search::text::{TextMatch, TextQuery};

use super::{FocusTarget, Workbench, file_name};
use crate::quick_open::{Action, Item, Mode, QuickOpen};

/// Results kept at most.
const MAX_RESULTS: usize = 500;
/// Quiet time after a keystroke before searching.
const PAUSE: Duration = Duration::from_millis(120);

/// Options of the search, toggled with Alt+C/W/X.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TextOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl Workbench {
    /// Opens the popup, starting from the word at the caret.
    pub(super) fn open_find_in_files(&mut self, cx: &mut Context<Self>) {
        let word = self
            .active_editor()
            .map(|e| (e.status)(cx).word)
            .unwrap_or_default();
        self.open_menu = None;
        self.quick_open = Some(QuickOpen::prompt(Mode::FindInFiles, &word));
        self.text_results.clear();
        self.refresh_quick();
        self.focus_request = Some(FocusTarget::Quick);
        self.start_text_search(cx);
    }

    /// Alt+C/W/X in the popup; `true` when the key was one of them.
    pub(super) fn toggle_text_option(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let options = &mut self.text_options;
        match key {
            "c" => options.case_sensitive = !options.case_sensitive,
            "w" => options.whole_word = !options.whole_word,
            "x" => options.regex = !options.regex,
            _ => return false,
        }
        self.start_text_search(cx);
        true
    }

    /// The options in the popup's title row: `Aa W .*`, active ones marked.
    pub(super) fn text_options_label(&self) -> String {
        let mark = |on: bool, label: &str| {
            if on {
                format!("[{label}]")
            } else {
                label.to_owned()
            }
        };
        let o = self.text_options;
        format!(
            "{} {} {}",
            mark(o.case_sensitive, "Aa"),
            mark(o.whole_word, "W"),
            mark(o.regex, ".*")
        )
    }

    pub(super) fn text_items(&self, query: &str) -> Vec<Item> {
        if query.is_empty() {
            return vec![self.note("findInFiles.prompt")];
        }
        if let Some(error) = &self.text_error {
            return vec![Item {
                label: error.lines().last().unwrap_or(error).to_owned(),
                highlights: Vec::new(),
                detail: String::new(),
                keys: None,
                action: Action::None,
            }];
        }
        self.text_results.clone()
    }

    /// Starts a search for the popup's query after a pause; an earlier one is dropped.
    pub(super) fn start_text_search(&mut self, cx: &mut Context<Self>) {
        self.text_generation += 1;
        let generation = self.text_generation;
        let Some(text) = self
            .quick_open
            .as_ref()
            .filter(|q| q.mode() == Mode::FindInFiles)
            .map(|q| q.query().to_owned())
        else {
            return;
        };
        let options = self.text_options;
        let query = TextQuery {
            text,
            case_sensitive: options.case_sensitive,
            whole_word: options.whole_word,
            regex: options.regex,
        };
        let core = self.core.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let current = this
                .update(cx, |w, _| w.text_generation == generation)
                .unwrap_or(false);
            if !current || query.text.is_empty() {
                let _ = this.update(cx, |w, cx| {
                    if w.text_generation == generation {
                        w.text_results.clear();
                        w.text_error = None;
                        w.refresh_quick();
                        cx.notify();
                    }
                });
                return;
            }
            let started = runtime
                .spawn(async move { core.search_text(&query, MAX_RESULTS) })
                .await;
            let mut found = match started {
                Ok(Ok(found)) => found,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |w, cx| {
                        w.text_error = Some(error);
                        w.refresh_quick();
                        cx.notify();
                    });
                    return;
                }
                Err(_) => return,
            };
            let _ = this.update(cx, |w, _| {
                w.text_error = None;
                w.text_results.clear();
            });
            // Batches: the list updates a few times a second, not per match.
            loop {
                let mut batch = Vec::new();
                let more = loop {
                    match found.try_recv() {
                        Ok(m) => {
                            batch.push(m);
                            if batch.len() >= 64 {
                                break true;
                            }
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break true,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break false,
                    }
                };
                let alive = this
                    .update(cx, |w, cx| {
                        if w.text_generation != generation {
                            return false;
                        }
                        if !batch.is_empty() {
                            w.text_results.extend(batch.into_iter().map(item));
                            w.refresh_quick_keeping_selection();
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive || !more {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
            }
        })
        .detach();
    }
}

/// A result row: the line with the match highlighted, the file and line number beside it.
fn item(m: TextMatch) -> Item {
    let text = m.text.trim_end();
    let trimmed = text.len() - text.trim_start().len();
    let shown = &text[trimmed..];
    // Highlighted chars: the match, in the trimmed line.
    let from = (m.range.start.saturating_sub(m.text_start)).saturating_sub(trimmed);
    let to = (m.range.end.saturating_sub(m.text_start)).saturating_sub(trimmed);
    let highlights = shown
        .char_indices()
        .enumerate()
        .filter(|(_, (byte, _))| *byte >= from && *byte < to)
        .map(|(i, _)| i as u32)
        .collect();
    let at = kumiko_types::Position::new(m.line, m.range.start as u32);
    Item {
        label: shown.to_owned(),
        highlights,
        detail: format!("{}:{}", file_name(&m.file.uri), m.line + 1),
        keys: None,
        action: Action::OpenAt(m.file.uri, at),
    }
}
