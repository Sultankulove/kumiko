//! A single-line text input as data: the text and a caret, edited by keys. Used by Quick Open
//! and the command palette; composition (IME) comes with the editor view (E8).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Caret position in characters.
    caret: usize,
    /// The whole text is selected: typing replaces it (a prompt with a suggestion).
    all_selected: bool,
}

impl LineInput {
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            caret: text.chars().count(),
            all_selected: false,
        }
    }

    pub fn select_all(&mut self) {
        self.all_selected = !self.text.is_empty();
        self.caret = self.len();
    }

    pub fn is_all_selected(&self) -> bool {
        self.all_selected
    }

    /// Drops a whole-text selection by deleting the text; `true` if there was one.
    fn take_selection(&mut self) -> bool {
        if std::mem::take(&mut self.all_selected) {
            self.text.clear();
            self.caret = 0;
            true
        } else {
            false
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text before and after the caret.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.byte(self.caret))
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn insert(&mut self, text: &str) {
        self.take_selection();
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &text);
        self.caret += text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.caret == 0 {
            return;
        }
        let (from, to) = (self.byte(self.caret - 1), self.byte(self.caret));
        self.text.replace_range(from..to, "");
        self.caret -= 1;
    }

    pub fn delete(&mut self) {
        if self.take_selection() {
            return;
        }
        if self.caret >= self.len() {
            return;
        }
        let (from, to) = (self.byte(self.caret), self.byte(self.caret + 1));
        self.text.replace_range(from..to, "");
    }

    pub fn left(&mut self) {
        self.all_selected = false;
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.all_selected = false;
        self.caret = (self.caret + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.all_selected = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all_selected = false;
        self.caret = self.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keeps_the_caret_on_character_boundaries() {
        let mut input = LineInput::default();
        input.insert("Прив");
        input.insert("ет\n");
        assert_eq!(input.text(), "Привет");
        input.left();
        input.left();
        input.backspace();
        assert_eq!(input.split(), ("При", "ет"));
        input.delete();
        assert_eq!(input.text(), "Прит");
        input.home();
        input.backspace();
        input.insert(">");
        input.end();
        input.right();
        input.delete();
        assert_eq!(input.split(), (">Прит", ""));
        assert_eq!(LineInput::with_text(":12").split(), (":12", ""));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn a_selected_suggestion_is_replaced_by_typing() {
        let mut input = LineInput::with_text("count");
        input.select_all();
        input.insert("a");
        assert_eq!(input.text(), "a");
        let mut input = LineInput::with_text("count");
        input.select_all();
        input.end();
        input.insert("er");
        assert_eq!(input.text(), "counter", "moving keeps the text");
        input.select_all();
        input.backspace();
        assert_eq!(input.text(), "");
    }
}
