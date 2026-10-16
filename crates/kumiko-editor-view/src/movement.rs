//! Caret movement, as in JetBrains IDEs: by graphemes, words, screen rows (through the
//! display map, remembering the column), smart Home, pages, document ends. With `extend`
//! the anchor stays (Shift), otherwise a selection collapses first.

use ropey::Rope;

use crate::display::DisplayMap;
use crate::selection::{Range, Selection};
use crate::text::{line_len, next_grapheme, prev_grapheme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    /// Smart Home: the first non-blank char, then the line start (row start when wrapped).
    LineStart,
    LineEnd,
    PageUp(usize),
    PageDown(usize),
    DocumentStart,
    DocumentEnd,
}

/// What kind of char, for word movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Word,
    Space,
    Punctuation,
    Newline,
}

pub fn class(c: char) -> CharClass {
    if c == '\n' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

pub fn move_selection(
    rope: &Rope,
    map: &DisplayMap,
    selection: &Selection,
    motion: Motion,
    extend: bool,
) -> Selection {
    selection.transform(|range| move_range(rope, map, range, motion, extend))
}

pub fn move_range(
    rope: &Rope,
    map: &DisplayMap,
    range: Range,
    motion: Motion,
    extend: bool,
) -> Range {
    // Left/Right on a selection without Shift: to its edge.
    if !extend && !range.is_empty() {
        match motion {
            Motion::Left => return Range::point(range.start()),
            Motion::Right => return Range::point(range.end()),
            _ => {}
        }
    }
    let head = range.head;
    let vertical = |rows: isize| -> Range {
        let (row, column) = map.locate(rope, head);
        let goal = range.goal.unwrap_or(column);
        let target = (row as isize + rows).clamp(0, map.row_count() as isize - 1) as usize;
        let mut moved = range.moved_to(map.char_at(rope, target, goal), extend);
        moved.goal = Some(goal);
        moved
    };
    let to = match motion {
        Motion::Left => prev_grapheme(rope, head),
        Motion::Right => next_grapheme(rope, head),
        Motion::WordLeft => word_left(rope, head),
        Motion::WordRight => word_right(rope, head),
        Motion::Up => return vertical(-1),
        Motion::Down => return vertical(1),
        Motion::PageUp(rows) => return vertical(-(rows.max(1) as isize)),
        Motion::PageDown(rows) => return vertical(rows.max(1) as isize),
        Motion::LineStart => line_start(rope, map, head),
        Motion::LineEnd => line_end(rope, map, head),
        Motion::DocumentStart => 0,
        Motion::DocumentEnd => rope.len_chars(),
    };
    range.moved_to(to, extend)
}

/// After the current word (or punctuation run), skipping blanks first; a line end is a stop.
fn word_right(rope: &Rope, at: usize) -> usize {
    let len = rope.len_chars();
    let mut pos = at;
    if pos < len && rope.char(pos) == '\n' {
        return pos + 1;
    }
    while pos < len && class(rope.char(pos)) == CharClass::Space {
        pos += 1;
    }
    if pos < len {
        let run = class(rope.char(pos));
        if run != CharClass::Newline {
            while pos < len && class(rope.char(pos)) == run {
                pos += 1;
            }
        }
    }
    pos
}

/// To the start of the previous word (or punctuation run), skipping blanks first.
fn word_left(rope: &Rope, at: usize) -> usize {
    let mut pos = at;
    if pos > 0 && rope.char(pos - 1) == '\n' {
        return pos - 1;
    }
    while pos > 0 && class(rope.char(pos - 1)) == CharClass::Space {
        pos -= 1;
    }
    if pos > 0 {
        let run = class(rope.char(pos - 1));
        if run != CharClass::Newline {
            while pos > 0 && class(rope.char(pos - 1)) == run {
                pos -= 1;
            }
        }
    }
    pos
}

fn line_start(rope: &Rope, map: &DisplayMap, head: usize) -> usize {
    let line = rope.char_to_line(head);
    let start = rope.line_to_char(line);
    let row = map.row(rope, map.locate(rope, head).0);
    // On a continuation row: first to where the row starts.
    if row.start > start && head != row.start {
        return row.start;
    }
    let first_blank_end = (start..start + line_len(rope, line))
        .find(|&c| !matches!(rope.char(c), ' ' | '\t'))
        .unwrap_or(start + line_len(rope, line));
    if head == first_blank_end {
        start
    } else {
        first_blank_end
    }
}

fn line_end(rope: &Rope, map: &DisplayMap, head: usize) -> usize {
    let line = rope.char_to_line(head);
    let end = rope.line_to_char(line) + line_len(rope, line);
    let (row_index, _) = map.locate(rope, head);
    let row = map.row(rope, row_index);
    // On a wrapped row: first to its end, then to the end of the line.
    if row.end < end {
        let row_end = map.char_at(rope, row_index, usize::MAX / 2);
        if head != row_end {
            return row_end;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayConfig;

    fn setup(text: &str) -> (Rope, DisplayMap) {
        let rope = Rope::from_str(text);
        let map = DisplayMap::new(&rope, DisplayConfig::default());
        (rope, map)
    }

    fn go(rope: &Rope, map: &DisplayMap, at: usize, motion: Motion) -> usize {
        move_range(rope, map, Range::point(at), motion, false).head
    }

    #[test]
    fn words_stop_at_runs_and_line_ends() {
        let (rope, map) = setup("let foo_bar = x.y;\nnext");
        assert_eq!(go(&rope, &map, 0, Motion::WordRight), 3);
        assert_eq!(go(&rope, &map, 3, Motion::WordRight), 11, "foo_bar");
        assert_eq!(go(&rope, &map, 11, Motion::WordRight), 13, "=");
        assert_eq!(
            go(&rope, &map, 18, Motion::WordRight),
            19,
            "across the line end"
        );
        assert_eq!(go(&rope, &map, 19, Motion::WordLeft), 18);
        assert_eq!(go(&rope, &map, 11, Motion::WordLeft), 4);
        assert_eq!(go(&rope, &map, 23, Motion::WordLeft), 19);
    }

    #[test]
    fn vertical_moves_keep_the_column() {
        let (rope, map) = setup("abcdefgh\nab\nabcdefgh");
        let start = Range::point(6);
        let down = move_range(&rope, &map, start, Motion::Down, false);
        assert_eq!(down.head, 11, "end of the short line");
        let down = move_range(&rope, &map, down, Motion::Down, false);
        assert_eq!(down.head, 18, "back to column 6");
        let up = move_range(&rope, &map, Range::point(2), Motion::Up, false);
        assert_eq!(up.head, 2, "first row: stays");
        let page = move_range(&rope, &map, Range::point(0), Motion::PageDown(10), false);
        assert_eq!(page.head, 12, "clamped to the last row");
    }

    #[test]
    fn smart_home_and_end() {
        let (rope, map) = setup("    code\n");
        assert_eq!(go(&rope, &map, 8, Motion::LineStart), 4);
        assert_eq!(go(&rope, &map, 4, Motion::LineStart), 0);
        assert_eq!(go(&rope, &map, 0, Motion::LineStart), 4);
        assert_eq!(go(&rope, &map, 0, Motion::LineEnd), 8);
    }

    #[test]
    fn home_and_end_on_wrapped_rows() {
        let rope = Rope::from_str("alpha beta gamma");
        let map = DisplayMap::new(
            &rope,
            DisplayConfig {
                tab_width: 4,
                wrap: Some(8),
            },
        );
        // Caret on "beta" (row 1, chars 6..11).
        assert_eq!(go(&rope, &map, 8, Motion::LineStart), 6, "row start first");
        assert_eq!(go(&rope, &map, 6, Motion::LineStart), 0, "then the line");
        assert_eq!(go(&rope, &map, 7, Motion::LineEnd), 10, "row end first");
        assert_eq!(
            go(&rope, &map, 10, Motion::LineEnd),
            16,
            "then the line end"
        );
    }

    #[test]
    fn shift_extends_and_plain_moves_collapse() {
        let (rope, map) = setup("hello");
        let selected = move_range(&rope, &map, Range::point(1), Motion::Right, true);
        assert_eq!(selected, Range::new(1, 2));
        assert_eq!(
            move_range(&rope, &map, Range::new(1, 4), Motion::Left, false),
            Range::point(1)
        );
        let all = move_range(&rope, &map, Range::point(2), Motion::DocumentEnd, true);
        assert_eq!(all, Range::new(2, 5));
    }
}
