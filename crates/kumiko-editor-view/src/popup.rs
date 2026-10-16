//! Popups at the caret: the completion list (filtered as you type, as in IntelliJ), the
//! intention actions of Alt+Enter, and quick documentation. Plain data, tested without UI.

use kumiko_core::language::{CodeAction, CompletionItem};

/// How well a label matches what was typed; `None` when it does not.
fn score(label: &str, typed: &str) -> Option<(u8, usize)> {
    if typed.is_empty() {
        return Some((0, 0));
    }
    if label.starts_with(typed) {
        return Some((0, label.len()));
    }
    let lower = label.to_lowercase();
    let typed_lower = typed.to_lowercase();
    if lower.starts_with(&typed_lower) {
        return Some((1, label.len()));
    }
    // Every typed char in order (camel humps, abbreviations).
    let mut rest = lower.chars();
    typed_lower
        .chars()
        .all(|t| rest.any(|c| c == t))
        .then_some((2, label.len()))
}

pub struct Completion {
    pub items: Vec<CompletionItem>,
    /// Indices of the items that match, best first.
    pub shown: Vec<usize>,
    pub selected: usize,
    /// Char where the word being completed starts.
    pub start: usize,
}

impl Completion {
    pub fn new(items: Vec<CompletionItem>, start: usize, typed: &str) -> Self {
        let mut completion = Self {
            items,
            shown: Vec::new(),
            selected: 0,
            start,
        };
        completion.filter(typed);
        completion
    }

    /// Keeps the items matching the typed prefix: exact-case prefixes, then any-case, then
    /// subsequences; the server's order within each.
    pub fn filter(&mut self, typed: &str) {
        let mut scored: Vec<((u8, usize), usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| score(&item.filter_text, typed).map(|s| ((s.0, i), i)))
            .collect();
        scored.sort_by_key(|(key, _)| *key);
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.shown.is_empty() {
            return;
        }
        let len = self.shown.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    pub fn current(&self) -> Option<&CompletionItem> {
        self.shown.get(self.selected).map(|&i| &self.items[i])
    }
}

pub struct Actions {
    pub actions: Vec<CodeAction>,
    pub selected: usize,
}

impl Actions {
    pub fn move_by(&mut self, delta: isize) {
        if self.actions.is_empty() {
            return;
        }
        let len = self.actions.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }
}

pub enum Popup {
    Completion(Completion),
    Actions(Actions),
    /// Quick documentation or a message ("No suggestions").
    Info(String),
    /// Parameter info: the signature, its active parameter as a byte range.
    Signature {
        label: String,
        active: Option<std::ops::Range<usize>>,
    },
}

#[cfg(test)]
mod tests {
    use kumiko_core::language::CompletionKind;

    use super::*;

    fn item(label: &str) -> CompletionItem {
        let mut item = CompletionItem::plain(label);
        item.kind = CompletionKind::Method;
        item
    }

    #[test]
    fn typing_narrows_and_ranks_prefixes_first() {
        let items = vec![
            item("toString"),
            item("getText"),
            item("trim"),
            item("Tree"),
        ];
        let mut completion = Completion::new(items, 0, "");
        assert_eq!(completion.shown.len(), 4);
        completion.filter("t");
        let labels: Vec<&str> = completion
            .shown
            .iter()
            .map(|&i| completion.items[i].label.as_str())
            .collect();
        assert_eq!(labels, ["toString", "trim", "Tree", "getText"]);
        completion.filter("tS");
        assert_eq!(completion.current().unwrap().label, "toString");
        completion.filter("xyz");
        assert!(completion.is_empty());
        assert!(completion.current().is_none());
    }

    #[test]
    fn selection_wraps() {
        let mut completion = Completion::new(vec![item("a"), item("b")], 0, "");
        completion.move_by(-1);
        assert_eq!(completion.current().unwrap().label, "b");
        completion.move_by(1);
        assert_eq!(completion.current().unwrap().label, "a");
    }
}
