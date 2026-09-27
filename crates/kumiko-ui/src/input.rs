//! A single-line text input as data: the text and a caret, edited by keys. Used by Quick Open
//! and the command palette; composition (IME) comes with the editor view (E8).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Caret position in characters.
    caret: usize,
}

impl LineInput {
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            caret: text.chars().count(),
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
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &text);
        self.caret += text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let (from, to) = (self.byte(self.caret - 1), self.byte(self.caret));
        self.text.replace_range(from..to, "");
        self.caret -= 1;
    }

    pub fn delete(&mut self) {
        if self.caret >= self.len() {
            return;
        }
        let (from, to) = (self.byte(self.caret), self.byte(self.caret + 1));
        self.text.replace_range(from..to, "");
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
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
