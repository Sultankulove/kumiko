//! The display map: document lines as rows on screen. Everything that asks "where is this
//! char on screen" or "which char is under this point" goes through it, so soft wrap (here)
//! and folding and inlay hints (later) change only this module.
//!
//! Soft wrap works in display columns of the monospace editor font (tabs to the next stop,
//! wide characters take two), so no text shaping is needed to place breaks. A row breaks
//! after the last whitespace that fits; a word longer than a row breaks where the row ends.

use ropey::Rope;

use crate::text::{self, column_of, line_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayConfig {
    pub tab_width: usize,
    /// Columns per row with soft wrap; `None` keeps every line on one row.
    pub wrap: Option<usize>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            wrap: None,
        }
    }
}

/// One screen row: a document line or a wrapped part of one, `start..end` in chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    /// Display column of `start` within its line (where a continuation row begins).
    pub start_column: usize,
}

#[derive(Debug, Clone)]
pub struct DisplayMap {
    config: DisplayConfig,
    /// First row of each line; one more entry holds the total.
    first_row: Vec<usize>,
    /// Char offsets (within the line) where each line's continuation rows start.
    breaks: Vec<Vec<usize>>,
}

impl DisplayMap {
    pub fn new(rope: &Rope, config: DisplayConfig) -> Self {
        let mut map = Self {
            config,
            first_row: Vec::new(),
            breaks: Vec::new(),
        };
        map.rebuild(rope);
        map
    }

    pub fn config(&self) -> DisplayConfig {
        self.config
    }

    /// Recomputes every row (after an edit or a change of width).
    pub fn rebuild(&mut self, rope: &Rope) {
        let lines = rope.len_lines();
        self.breaks = (0..lines)
            .map(|line| match self.config.wrap {
                Some(width) => wrap_line(rope, line, width.max(1), self.config.tab_width),
                None => Vec::new(),
            })
            .collect();
        self.first_row = Vec::with_capacity(lines + 1);
        let mut row = 0;
        for breaks in &self.breaks {
            self.first_row.push(row);
            row += breaks.len() + 1;
        }
        self.first_row.push(row);
    }

    pub fn set_config(&mut self, rope: &Rope, config: DisplayConfig) {
        if config != self.config {
            self.config = config;
            self.rebuild(rope);
        }
    }

    pub fn row_count(&self) -> usize {
        self.first_row.last().copied().unwrap_or(1).max(1)
    }

    /// The screen row at `index` (clamped to the last one).
    pub fn row(&self, rope: &Rope, index: usize) -> Row {
        let index = index.min(self.row_count() - 1);
        // The line whose rows contain `index`: the last first_row <= index.
        let line = self
            .first_row
            .partition_point(|&first| first <= index)
            .saturating_sub(1)
            .min(self.breaks.len().saturating_sub(1));
        let part = index - self.first_row[line];
        let line_start = rope.line_to_char(line);
        let breaks = &self.breaks[line];
        let start = if part == 0 { 0 } else { breaks[part - 1] };
        let end = breaks.get(part).copied().unwrap_or(line_len(rope, line));
        Row {
            line,
            start: line_start + start,
            end: line_start + end,
            start_column: column_of(rope, line_start + start, self.config.tab_width),
        }
    }

    /// The row showing a char and its display column within that row. A char at a wrap
    /// point belongs to the row it starts.
    pub fn locate(&self, rope: &Rope, char: usize) -> (usize, usize) {
        let char = char.min(rope.len_chars());
        let line = rope.char_to_line(char);
        let within = char - rope.line_to_char(line);
        let part = self.breaks[line].partition_point(|&b| b <= within);
        let row = self.first_row[line] + part;
        let start_column = self.row(rope, row).start_column;
        (
            row,
            column_of(rope, char, self.config.tab_width) - start_column,
        )
    }

    /// The char at a display column of a row (the end of the row past its text).
    pub fn char_at(&self, rope: &Rope, row: usize, column: usize) -> usize {
        let row = self.row(rope, row);
        let found = text::char_at_column(
            rope,
            row.line,
            row.start_column + column,
            self.config.tab_width,
        );
        // A wrapped row ends where the next one starts; its caret stays on this row.
        let last = if row.end < rope.line_to_char(row.line) + line_len(rope, row.line) {
            row.end.saturating_sub(1).max(row.start)
        } else {
            row.end
        };
        found.clamp(row.start, last)
    }
}

/// Break points of one line for rows of `width` columns.
fn wrap_line(rope: &Rope, line: usize, width: usize, tab_width: usize) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    let text: String = rope.line(line).chars().take(line_len(rope, line)).collect();
    let mut breaks = Vec::new();
    let mut column = 0; // within the line
    let mut row_start_column = 0;
    let mut chars = 0;
    // The last place a row may break: after whitespace (chars, column there).
    let mut last_space: Option<(usize, usize)> = None;
    for grapheme in text.graphemes(true) {
        let grapheme_width = if grapheme == "\t" {
            tab_width.max(1) - column % tab_width.max(1)
        } else {
            unicode_width::UnicodeWidthStr::width(grapheme).max(1)
        };
        if column + grapheme_width - row_start_column > width
            && chars > breaks.last().copied().unwrap_or(0)
        {
            let (at, at_column) = match last_space {
                Some(space) if space.0 > breaks.last().copied().unwrap_or(0) => space,
                _ => (chars, column),
            };
            breaks.push(at);
            row_start_column = at_column;
            last_space = None;
        }
        column += grapheme_width;
        chars += grapheme.chars().count();
        if grapheme.chars().all(char::is_whitespace) {
            last_space = Some((chars, column));
        }
    }
    breaks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rope: &Rope, map: &DisplayMap) -> Vec<String> {
        (0..map.row_count())
            .map(|i| {
                let row = map.row(rope, i);
                rope.slice(row.start..row.end).to_string()
            })
            .collect()
    }

    #[test]
    fn without_wrap_rows_are_lines() {
        let rope = Rope::from_str("one\ntwo\n");
        let map = DisplayMap::new(&rope, DisplayConfig::default());
        assert_eq!(rows(&rope, &map), ["one", "two", ""]);
        assert_eq!(map.locate(&rope, 5), (1, 1));
        assert_eq!(map.char_at(&rope, 1, 99), 7);
    }

    #[test]
    fn long_lines_wrap_after_spaces_or_hard() {
        let rope = Rope::from_str("alpha beta gamma\nxxxxxxxxxxxx\nshort");
        let config = DisplayConfig {
            tab_width: 4,
            wrap: Some(8),
        };
        let map = DisplayMap::new(&rope, config);
        assert_eq!(
            rows(&rope, &map),
            ["alpha ", "beta ", "gamma", "xxxxxxxx", "xxxx", "short"]
        );
        // "beta" starts row 1; its "t" is column 2 there.
        assert_eq!(map.locate(&rope, 8), (1, 2));
        assert_eq!(map.char_at(&rope, 1, 2), 8);
        // Past the end of a wrapped row: its last char, not the next row's first.
        assert_eq!(map.char_at(&rope, 0, 99), 5);
        assert_eq!(map.char_at(&rope, 2, 99), 16, "end of the line");
        assert_eq!(map.row(&rope, 99).line, 2, "clamped to the last row");
    }

    #[test]
    fn wide_characters_and_tabs_count_by_columns() {
        let rope = Rope::from_str("日本語日本語\n\tab");
        let config = DisplayConfig {
            tab_width: 4,
            wrap: Some(5),
        };
        let map = DisplayMap::new(&rope, config);
        // The tab is whitespace: the row breaks after it rather than inside "ab".
        assert_eq!(rows(&rope, &map), ["日本", "語日", "本語", "\t", "ab"]);
    }
}
