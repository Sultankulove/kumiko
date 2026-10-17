//! Quick Open and the command palette (spec, section 20): one popup whose input prefix picks
//! the mode, as in the spec: `>` commands, `:` a line, `@` document symbols, anything else a
//! file. The workbench fills [`QuickOpen::items`] for the query; this is the state and the
//! mode logic.

use kumiko_types::{CommandId, Uri};

use crate::input::LineInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Files,
    Commands,
    Line,
    Symbols,
    /// A fixed list to pick from (no prefix): colour themes.
    Themes,
    /// Usages of a symbol (Alt+F7), filtered by what is typed.
    Usages,
    /// The new name of a symbol (Shift+F6): the input is the answer.
    Rename,
    /// Tasks to run (Alt+Shift+F10).
    Tasks,
    /// Text in the project's files (Ctrl+Shift+F): the input is the query.
    FindInFiles,
}

impl Mode {
    /// The prefix that selects the mode.
    pub fn prefix(self) -> &'static str {
        match self {
            Mode::Files => "",
            Mode::Commands => ">",
            Mode::Line => ":",
            Mode::Symbols => "@",
            Mode::Themes | Mode::Usages | Mode::Rename | Mode::Tasks | Mode::FindInFiles => "",
        }
    }
}

/// What choosing an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    OpenFile(Uri),
    Command(CommandId),
    GotoLine(usize),
    /// Make the theme with this id the current one.
    SetTheme(String),
    /// Open a file at a position (usages, symbols).
    OpenAt(Uri, kumiko_types::Position),
    /// Rename to what was typed.
    Rename,
    /// Run the task with this index of the offered list.
    RunTask(usize),
    /// Informational row (e.g. "no results").
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// Character positions of `label` that matched the query.
    pub highlights: Vec<u32>,
    /// Secondary text: the folder of a file, the id of a command.
    pub detail: String,
    pub keys: Option<String>,
    pub action: Action,
}

#[derive(Debug, Clone, Default)]
pub struct QuickOpen {
    pub input: LineInput,
    pub items: Vec<Item>,
    pub selected: usize,
    /// A picker keeps its mode whatever is typed.
    fixed: Option<Mode>,
}

impl QuickOpen {
    /// Opens in a mode: the input starts with its prefix.
    pub fn new(mode: Mode) -> Self {
        Self {
            input: LineInput::with_text(mode.prefix()),
            ..Self::default()
        }
    }

    /// Opens a picker: the input filters a fixed list.
    pub fn picker(mode: Mode) -> Self {
        Self {
            fixed: Some(mode),
            ..Self::default()
        }
    }

    /// Opens a prompt: a fixed mode whose input starts with `text`.
    pub fn prompt(mode: Mode, text: &str) -> Self {
        Self {
            input: {
                let mut input = LineInput::with_text(text);
                input.select_all();
                input
            },
            fixed: Some(mode),
            ..Self::default()
        }
    }

    pub fn mode(&self) -> Mode {
        if let Some(mode) = self.fixed {
            return mode;
        }
        match self.input.text().chars().next() {
            Some('>') => Mode::Commands,
            Some(':') => Mode::Line,
            Some('@') => Mode::Symbols,
            _ => Mode::Files,
        }
    }

    /// The query without the mode prefix.
    pub fn query(&self) -> &str {
        let text = self.input.text();
        text.strip_prefix(self.mode().prefix())
            .unwrap_or(text)
            .trim()
    }

    /// New results; the selection goes back to the first one.
    pub fn set_items(&mut self, items: Vec<Item>) {
        self.items = items;
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }
}

/// `:12` → line 12 (1-based); nothing for text that is not a positive number.
pub fn parse_line(query: &str) -> Option<usize> {
    query.parse::<usize>().ok().filter(|&n| n > 0)
}

/// Positions matched in a path, shifted onto its file name (dropping those in the folders).
pub fn name_highlights(relative: &str, name: &str, indices: &[u32]) -> Vec<u32> {
    let offset = (relative.chars().count() - name.chars().count()) as u32;
    indices
        .iter()
        .filter(|&&i| i >= offset)
        .map(|&i| i - offset)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_selects_the_mode() {
        let mut quick = QuickOpen::new(Mode::Files);
        assert_eq!((quick.mode(), quick.query()), (Mode::Files, ""));
        quick.input.insert("> toggle ");
        assert_eq!((quick.mode(), quick.query()), (Mode::Commands, "toggle"));
        let quick = QuickOpen::new(Mode::Line);
        assert_eq!(quick.input.text(), ":");
        assert_eq!(quick.mode(), Mode::Line);
        assert_eq!(QuickOpen::new(Mode::Symbols).mode(), Mode::Symbols);
        let mut picker = QuickOpen::picker(Mode::Themes);
        picker.input.insert(">dar");
        assert_eq!((picker.mode(), picker.query()), (Mode::Themes, ">dar"));
    }

    #[test]
    fn selection_stays_within_the_items() {
        let mut quick = QuickOpen::new(Mode::Files);
        quick.move_selection(1);
        assert_eq!(quick.selected, 0);
        let item = |n: usize| Item {
            label: n.to_string(),
            highlights: vec![],
            detail: String::new(),
            keys: None,
            action: Action::GotoLine(n),
        };
        quick.set_items(vec![item(1), item(2), item(3)]);
        quick.move_selection(5);
        assert_eq!(quick.items[quick.selected].action, Action::GotoLine(3));
        quick.move_selection(-1);
        assert_eq!(quick.selected, 1);
        quick.set_items(vec![item(7)]);
        assert_eq!(quick.selected, 0);
    }

    #[test]
    fn lines_and_name_highlights() {
        assert_eq!(parse_line("12"), Some(12));
        assert_eq!(parse_line("0"), None);
        assert_eq!(parse_line("x"), None);
        // "src/App.txt": the name starts at 4.
        assert_eq!(
            name_highlights("src/App.txt", "App.txt", &[0, 4, 5]),
            [0, 1]
        );
    }
}
