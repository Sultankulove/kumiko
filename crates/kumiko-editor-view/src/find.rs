//! The find/replace bar above the editor, as in JetBrains IDEs: a query field, a replacement
//! field, case / whole-word / regex toggles (Alt+C, Alt+W, Alt+X), the match count.

use crate::search::Query;

/// A single-line field: its text and caret (in chars).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Field {
    pub text: String,
    pub caret: usize,
}

impl Field {
    pub fn with(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            caret: text.chars().count(),
        }
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.byte(self.caret))
    }

    /// Applies an editing key; `true` when the text changed.
    pub fn key(&mut self, key: &str, typed: Option<&str>) -> bool {
        let len = self.text.chars().count();
        match key {
            "backspace" if self.caret > 0 => {
                let (from, to) = (self.byte(self.caret - 1), self.byte(self.caret));
                self.text.replace_range(from..to, "");
                self.caret -= 1;
                true
            }
            "delete" if self.caret < len => {
                let (from, to) = (self.byte(self.caret), self.byte(self.caret + 1));
                self.text.replace_range(from..to, "");
                true
            }
            "left" => {
                self.caret = self.caret.saturating_sub(1);
                false
            }
            "right" => {
                self.caret = (self.caret + 1).min(len);
                false
            }
            "home" => {
                self.caret = 0;
                false
            }
            "end" => {
                self.caret = len;
                false
            }
            _ => match typed.filter(|t| !t.chars().any(char::is_control)) {
                Some(text) => {
                    let at = self.byte(self.caret);
                    self.text.insert_str(at, text);
                    self.caret += text.chars().count();
                    true
                }
                None => false,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Find,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindBar {
    pub find: Field,
    pub replace: Field,
    pub replacing: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub focus: Focus,
    /// Matches as char ranges, recomputed when the query or the text changes.
    pub matches: Vec<(usize, usize)>,
    /// The match the selection is on, if any.
    pub current: Option<usize>,
    /// An invalid regular expression.
    pub error: Option<String>,
}

impl FindBar {
    pub fn new(text: &str, replacing: bool) -> Self {
        Self {
            find: Field::with(text),
            replace: Field::default(),
            replacing,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            focus: Focus::Find,
            matches: Vec::new(),
            current: None,
            error: None,
        }
    }

    pub fn query(&self) -> Query {
        Query {
            text: self.find.text.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    pub fn field(&mut self) -> &mut Field {
        match self.focus {
            Focus::Find => &mut self.find,
            Focus::Replace => &mut self.replace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_edit_by_chars() {
        let mut field = Field::default();
        assert!(field.key("x", Some("при")));
        assert!(field.key("x", Some("вет")));
        assert!(!field.key("left", None));
        assert!(field.key("backspace", None));
        assert_eq!(field.split(), ("прив", "т"));
        assert!(!field.key("x", Some("\u{8}")), "control text is not typed");
        assert!(field.key("delete", None));
        assert_eq!(field.text, "прив");
        let bar = FindBar::new("foo", true);
        assert_eq!(bar.query().text, "foo");
        assert_eq!(bar.find.caret, 3);
    }
}
