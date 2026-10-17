//! Positions in a document as the editor uses them: char offsets into the rope. Conversions
//! to the protocol's `Position` (line and UTF-8 byte), to UTF-16 (IME), grapheme boundaries
//! and display columns (tabs, wide characters) live here.
//!
//! Buffers hold `\n` line endings only (the Document Service normalizes them).

use kumiko_types::Position;
use ropey::{Rope, RopeSlice};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::UnicodeWidthStr;

/// Characters of a line without its line break.
pub fn line_len(rope: &Rope, line: usize) -> usize {
    let slice = rope.line(line);
    let len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len - 1
    } else {
        len
    }
}

/// The last line index (a trailing `\n` starts an empty last line).
pub fn last_line(rope: &Rope) -> usize {
    rope.len_lines().saturating_sub(1)
}

/// Char offset of a protocol position, clamped into the document (a column past the end of
/// its line lands at the end, one inside a character at its start).
pub fn char_of(rope: &Rope, position: Position) -> usize {
    let line = (position.line as usize).min(last_line(rope));
    let start = rope.line_to_char(line);
    let slice = rope.line(line);
    let byte = (position.character as usize).min(slice.len_bytes());
    let within = slice.byte_to_char(byte);
    start + within.min(line_len(rope, line))
}

/// Protocol position of a char offset.
pub fn position_of(rope: &Rope, char: usize) -> Position {
    let char = char.min(rope.len_chars());
    let line = rope.char_to_line(char);
    let byte = rope.char_to_byte(char) - rope.line_to_byte(line);
    Position::new(line as u32, byte as u32)
}

/// UTF-16 code units before a char offset (GPUI's input handler counts in UTF-16).
pub fn utf16_of(rope: &Rope, char: usize) -> usize {
    rope.char_to_utf16_cu(char.min(rope.len_chars()))
}

/// Char offset of a UTF-16 offset, clamped.
pub fn char_of_utf16(rope: &Rope, utf16: usize) -> usize {
    rope.utf16_cu_to_char(utf16.min(rope.len_utf16_cu()))
}

/// The grapheme boundary after `char` (the end of the document stays put).
pub fn next_grapheme(rope: &Rope, char: usize) -> usize {
    grapheme_step(rope.slice(..), char, true)
}

/// The grapheme boundary before `char`.
pub fn prev_grapheme(rope: &Rope, char: usize) -> usize {
    grapheme_step(rope.slice(..), char, false)
}

/// One grapheme step over rope chunks, as in ropey's grapheme example.
fn grapheme_step(slice: RopeSlice, char: usize, forward: bool) -> usize {
    let char = char.min(slice.len_chars());
    if (forward && char == slice.len_chars()) || (!forward && char == 0) {
        return char;
    }
    let byte = slice.char_to_byte(char);
    let (mut chunk, mut chunk_byte, _, _) = slice.chunk_at_byte(byte);
    let mut cursor = GraphemeCursor::new(byte, slice.len_bytes(), true);
    loop {
        let step = if forward {
            cursor.next_boundary(chunk, chunk_byte)
        } else {
            cursor.prev_boundary(chunk, chunk_byte)
        };
        match step {
            Ok(None) => return if forward { slice.len_chars() } else { 0 },
            Ok(Some(boundary)) => return slice.byte_to_char(boundary),
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte += chunk.len();
                let (next, start, _, _) = slice.chunk_at_byte(chunk_byte);
                chunk = next;
                chunk_byte = start;
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                let (prev, start, _, _) = slice.chunk_at_byte(chunk_byte - 1);
                chunk = prev;
                chunk_byte = start;
            }
            Err(GraphemeIncomplete::PreContext(at)) => {
                let (context, start, _, _) = slice.chunk_at_byte(at - 1);
                cursor.provide_context(context, start);
            }
            // Only for a cursor whose position is out of range, which cannot happen here.
            Err(GraphemeIncomplete::InvalidOffset) => return char,
        }
    }
}

/// Display width of a grapheme: a tab reaches the next tab stop, wide characters take two
/// columns, control characters one.
fn grapheme_width(grapheme: &str, column: usize, tab_width: usize) -> usize {
    if grapheme == "\t" {
        let tab = tab_width.max(1);
        tab - column % tab
    } else {
        grapheme.width().max(1)
    }
}

/// Graphemes of a line (without its break) with their char offset in the line.
fn graphemes(rope: &Rope, line: usize) -> Vec<(usize, String)> {
    use unicode_segmentation::UnicodeSegmentation;
    let text: String = rope.line(line).chars().take(line_len(rope, line)).collect();
    let mut chars = 0;
    text.graphemes(true)
        .map(|g| {
            let at = chars;
            chars += g.chars().count();
            (at, g.to_owned())
        })
        .collect()
}

/// Display column of a char offset within its line.
pub fn column_of(rope: &Rope, char: usize, tab_width: usize) -> usize {
    let char = char.min(rope.len_chars());
    let line = rope.char_to_line(char);
    let within = char - rope.line_to_char(line);
    let mut column = 0;
    for (at, grapheme) in graphemes(rope, line) {
        if at >= within {
            break;
        }
        column += grapheme_width(&grapheme, column, tab_width);
    }
    column
}

/// Char offset on `line` closest to a display column without passing it (a column inside a
/// wide character or tab lands before it); past the end of the line, its end.
pub fn char_at_column(rope: &Rope, line: usize, target: usize, tab_width: usize) -> usize {
    let line = line.min(last_line(rope));
    let start = rope.line_to_char(line);
    let mut column = 0;
    for (at, grapheme) in graphemes(rope, line) {
        let width = grapheme_width(&grapheme, column, tab_width);
        if column + width > target {
            // Closer to the end of the grapheme: after it.
            return if target - column > width / 2 && width > 1 {
                start + at + grapheme.chars().count()
            } else {
                start + at
            };
        }
        column += width;
    }
    start + line_len(rope, line)
}

/// Display width of a whole line.
pub fn line_width(rope: &Rope, line: usize, tab_width: usize) -> usize {
    let end = rope.line_to_char(line) + line_len(rope, line);
    column_of(rope, end, tab_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_convert_both_ways_and_clamp() {
        let rope = Rope::from_str("aé\nПривет\n");
        assert_eq!(char_of(&rope, Position::new(0, 3)), 2, "after é (2 bytes)");
        assert_eq!(char_of(&rope, Position::new(1, 4)), 5, "П and р");
        assert_eq!(char_of(&rope, Position::new(1, 99)), 9, "end of the line");
        assert_eq!(
            char_of(&rope, Position::new(9, 0)),
            10,
            "the empty last line"
        );
        for char in 0..=rope.len_chars() {
            assert_eq!(char_of(&rope, position_of(&rope, char)), char);
        }
        assert_eq!(position_of(&rope, 5), Position::new(1, 4));
    }

    #[test]
    fn utf16_counts_surrogate_pairs() {
        let rope = Rope::from_str("a😀b");
        assert_eq!(utf16_of(&rope, 2), 3);
        assert_eq!(char_of_utf16(&rope, 3), 2);
        assert_eq!(char_of_utf16(&rope, 99), 3);
    }

    #[test]
    fn grapheme_steps_keep_clusters_together() {
        // e + combining acute, a family emoji (one grapheme, several chars), then a newline.
        let text = "e\u{301}👨‍👩‍👧x\n";
        let rope = Rope::from_str(text);
        let family_end = 2 + "👨‍👩‍👧".chars().count();
        assert_eq!(next_grapheme(&rope, 0), 2);
        assert_eq!(next_grapheme(&rope, 2), family_end);
        assert_eq!(prev_grapheme(&rope, family_end), 2);
        assert_eq!(prev_grapheme(&rope, 2), 0);
        assert_eq!(prev_grapheme(&rope, 0), 0);
        let end = rope.len_chars();
        assert_eq!(next_grapheme(&rope, end), end);
        assert_eq!(next_grapheme(&rope, end - 1), end, "the newline");
    }

    #[test]
    fn grapheme_steps_cross_rope_chunks() {
        // Long enough for several chunks; every step is one char here.
        let text = "ab".repeat(5000);
        let rope = Rope::from_str(&text);
        let mut at = 0;
        let mut steps = 0;
        while at < rope.len_chars() {
            at = next_grapheme(&rope, at);
            steps += 1;
        }
        assert_eq!(steps, 10_000);
        assert_eq!(prev_grapheme(&rope, 7777), 7776);
    }

    #[test]
    fn columns_follow_tabs_and_wide_characters() {
        let rope = Rope::from_str("\tab\n日本x\n");
        assert_eq!(column_of(&rope, 1, 4), 4, "after the tab");
        assert_eq!(column_of(&rope, 3, 4), 6);
        assert_eq!(line_width(&rope, 1, 4), 5, "two wide characters and x");
        assert_eq!(
            char_at_column(&rope, 0, 2, 4),
            0,
            "inside the tab: before it"
        );
        assert_eq!(
            char_at_column(&rope, 0, 3, 4),
            1,
            "nearer its end: after it"
        );
        assert_eq!(char_at_column(&rope, 1, 1, 4), 4, "inside 日");
        assert_eq!(char_at_column(&rope, 1, 2, 4), 5);
        assert_eq!(char_at_column(&rope, 1, 99, 4), 7, "past the end");
    }
}
