//! Positions and edits on a rope with UTF-8 based coordinates (ADR-012).
//!
//! Buffers keep `\n` line endings only; the file's own endings are restored on save.

use ropey::Rope;

use kumiko_types::{Position, Range, TextEdit};

use crate::DocError;

/// Replaces `\r\n` and lone `\r` with `\n`.
pub(crate) fn normalize_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Position right after `text` inserted at `start`.
pub(crate) fn end_after(start: Position, text: &str) -> Position {
    match text.rfind('\n') {
        None => Position::new(start.line, start.character + text.len() as u32),
        Some(last) => Position::new(
            start.line + text.matches('\n').count() as u32,
            (text.len() - last - 1) as u32,
        ),
    }
}

/// Maps a position at or after the end of `edit` into the text after the edit.
fn past(position: Position, edit: &TextEdit) -> Position {
    let Range { start, end } = edit.range;
    let new_end = end_after(start, &edit.new_text);
    if position.line == end.line {
        Position::new(
            new_end.line,
            new_end.character + (position.character - end.character),
        )
    } else {
        let line = i64::from(position.line) + i64::from(new_end.line) - i64::from(end.line);
        Position::new(line as u32, position.character)
    }
}

/// Maps a position through an edit applied before it. A position at the start of the edit
/// stays in front of the new text; `None` if it lies strictly inside the replaced range.
pub(crate) fn shift(position: Position, edit: &TextEdit) -> Option<Position> {
    if position <= edit.range.start {
        Some(position)
    } else if position < edit.range.end {
        None
    } else {
        Some(past(position, edit))
    }
}

/// Rebases an edit computed against an older version through a later transaction, whose
/// edits are listed in application order (descending). `None` on a conflict, i.e. when the
/// ranges overlap. Text inserted later at the start of the edit ends up in front of it.
pub(crate) fn rebase(edit: &TextEdit, later: &[TextEdit]) -> Option<TextEdit> {
    let mut range = edit.range;
    for other in later {
        let (start, end) = (other.range.start, other.range.end);
        let touches = range.end == start && !(range.is_empty() && other.range.is_empty());
        range = if range.end < start || touches {
            // Entirely before the other edit. Only two inserts at one point move: the
            // later one's text stays in front.
            range
        } else if range.start >= end {
            Range::new(past(range.start, other), past(range.end, other))
        } else {
            return None;
        };
    }
    Some(TextEdit {
        range,
        new_text: edit.new_text.clone(),
    })
}

/// Char index of a position, validating line, length and UTF-8 boundaries.
pub(crate) fn char_index(rope: &Rope, position: Position) -> Result<usize, DocError> {
    let invalid = || DocError::InvalidPosition(position);
    let line = position.line as usize;
    if line >= rope.len_lines() {
        return Err(invalid());
    }
    let line_start = rope.line_to_byte(line);
    let line_slice = rope.line(line);
    let mut line_len = line_slice.len_bytes();
    if line_len > 0 && line_slice.byte(line_len - 1) == b'\n' {
        line_len -= 1;
    }
    let character = position.character as usize;
    if character > line_len {
        return Err(invalid());
    }
    let byte = line_start + character;
    let char_idx = rope.byte_to_char(byte);
    if rope.char_to_byte(char_idx) != byte {
        return Err(invalid());
    }
    Ok(char_idx)
}

/// An applied transaction: its edits in application order and the inverse transaction.
pub(crate) struct Applied {
    /// Descending by position; applying them in order to the previous text reproduces it.
    pub edits: Vec<TextEdit>,
    /// Undoes the transaction; relative to the new text.
    pub inverse: Vec<TextEdit>,
}

/// Applies non-overlapping edits relative to the current text as one transaction.
pub(crate) fn apply(rope: &mut Rope, edits: Vec<TextEdit>) -> Result<Applied, DocError> {
    let mut edits: Vec<TextEdit> = edits
        .into_iter()
        .map(|e| TextEdit {
            range: e.range,
            new_text: normalize_newlines(&e.new_text),
        })
        .collect();
    for edit in &edits {
        if edit.range.start > edit.range.end {
            return Err(DocError::InvalidRange(edit.range));
        }
    }
    // Stable: inserts at the same point keep their given order.
    edits.sort_by_key(|edit| edit.range.start);
    for pair in edits.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            return Err(DocError::OverlappingEdits);
        }
    }

    let mut resolved = Vec::with_capacity(edits.len());
    for edit in &edits {
        let start = char_index(rope, edit.range.start)?;
        let end = char_index(rope, edit.range.end)?;
        resolved.push((start, end));
    }

    // Inverse edits in coordinates of the final text.
    let mut inverse = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let mut start = edit.range.start;
        for earlier in edits[..i].iter().rev() {
            start = shift(start, earlier).ok_or(DocError::OverlappingEdits)?;
        }
        let (from, to) = resolved[i];
        inverse.push(TextEdit {
            range: Range::new(start, end_after(start, &edit.new_text)),
            new_text: rope.slice(from..to).to_string(),
        });
    }

    for (edit, (from, to)) in edits.iter().zip(&resolved).rev() {
        rope.remove(*from..*to);
        rope.insert(*from, &edit.new_text);
    }
    edits.reverse();
    Ok(Applied { edits, inverse })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(l1: u32, c1: u32, l2: u32, c2: u32, text: &str) -> TextEdit {
        TextEdit {
            range: Range::new(Position::new(l1, c1), Position::new(l2, c2)),
            new_text: text.to_owned(),
        }
    }

    fn applied(text: &str, edits: Vec<TextEdit>) -> (String, Applied) {
        let mut rope = Rope::from_str(text);
        let result = apply(&mut rope, edits).unwrap();
        (rope.to_string(), result)
    }

    #[test]
    fn positions_are_utf8_bytes() {
        let rope = Rope::from_str("aé€\nx");
        assert_eq!(char_index(&rope, Position::new(0, 3)).unwrap(), 2); // after "aé"
        assert_eq!(char_index(&rope, Position::new(0, 6)).unwrap(), 3); // end of line
        assert!(matches!(
            char_index(&rope, Position::new(0, 2)),
            Err(DocError::InvalidPosition(_))
        ));
        assert!(
            char_index(&rope, Position::new(0, 7)).is_err(),
            "past the line end"
        );
        assert_eq!(char_index(&rope, Position::new(1, 1)).unwrap(), 5); // a é € \n x
        assert!(char_index(&rope, Position::new(2, 0)).is_err());
    }

    #[test]
    fn multiple_edits_and_their_inverse() {
        let original = "let a = 1;\nlet b = 2;\n";
        let (text, result) = applied(
            original,
            vec![
                edit(1, 4, 1, 5, "beta"),
                edit(0, 4, 0, 5, "alpha\nx"),
                edit(0, 0, 0, 0, "// hi\n"),
            ],
        );
        assert_eq!(text, "// hi\nlet alpha\nx = 1;\nlet beta = 2;\n");
        let starts: Vec<_> = result.edits.iter().map(|e| e.range.start.line).collect();
        assert_eq!(starts, [1, 0, 0], "application order is descending");

        let mut rope = Rope::from_str(&text);
        apply(&mut rope, result.inverse).unwrap();
        assert_eq!(rope.to_string(), original);
    }

    #[test]
    fn inserts_at_one_point_keep_their_order() {
        let (text, _) = applied("ab", vec![edit(0, 1, 0, 1, "X"), edit(0, 1, 0, 1, "Y")]);
        assert_eq!(text, "aXYb");
    }

    #[test]
    fn overlapping_and_reversed_ranges_are_rejected() {
        let mut rope = Rope::from_str("abcdef");
        assert!(matches!(
            apply(
                &mut rope,
                vec![edit(0, 0, 0, 3, "x"), edit(0, 2, 0, 4, "y")]
            ),
            Err(DocError::OverlappingEdits)
        ));
        assert!(matches!(
            apply(&mut rope, vec![edit(0, 3, 0, 1, "x")]),
            Err(DocError::InvalidRange(_))
        ));
        assert_eq!(
            rope.to_string(),
            "abcdef",
            "failed transactions change nothing"
        );
    }

    #[test]
    fn newlines_in_edits_are_normalized() {
        let (text, result) = applied("a", vec![edit(0, 1, 0, 1, "\r\nb\rc")]);
        assert_eq!(text, "a\nb\nc");
        assert_eq!(result.edits[0].new_text, "\nb\nc");
    }

    #[test]
    fn shifting_positions() {
        let e = edit(1, 2, 1, 4, "xy\nz");
        assert_eq!(shift(Position::new(0, 9), &e), Some(Position::new(0, 9)));
        assert_eq!(shift(Position::new(1, 2), &e), Some(Position::new(1, 2)));
        assert_eq!(shift(Position::new(1, 3), &e), None);
        assert_eq!(shift(Position::new(1, 6), &e), Some(Position::new(2, 3)));
        assert_eq!(shift(Position::new(3, 1), &e), Some(Position::new(4, 1)));
        let join = edit(0, 3, 2, 1, "");
        assert_eq!(shift(Position::new(2, 5), &join), Some(Position::new(0, 7)));
        assert_eq!(shift(Position::new(5, 0), &join), Some(Position::new(3, 0)));
    }

    #[test]
    fn rebasing_through_later_transactions() {
        let later = vec![edit(0, 0, 0, 0, "// header\n")];
        let stale = edit(2, 0, 2, 3, "new");
        assert_eq!(rebase(&stale, &later).unwrap(), edit(3, 0, 3, 3, "new"));

        let before = vec![edit(5, 0, 5, 1, "zzz")];
        assert_eq!(
            rebase(&stale, &before).unwrap(),
            stale,
            "edits after it do not move it"
        );

        let overlapping = vec![edit(2, 1, 2, 2, "q")];
        assert!(rebase(&stale, &overlapping).is_none());

        let same_point = vec![edit(1, 0, 1, 0, "a")];
        let insert = edit(1, 0, 1, 0, "b");
        assert_eq!(rebase(&insert, &same_point).unwrap(), edit(1, 1, 1, 1, "b"));

        // A later insert at the end of the edit leaves it alone, one at its start moves it.
        let at_end = vec![edit(2, 3, 2, 3, "!")];
        assert_eq!(rebase(&stale, &at_end).unwrap(), stale);
        let at_start = vec![edit(2, 0, 2, 0, ">>")];
        assert_eq!(rebase(&stale, &at_start).unwrap(), edit(2, 2, 2, 5, "new"));
        // An insert right before a later replacement stays before it.
        let replaced = vec![edit(2, 0, 2, 3, "xyz")];
        let before = edit(2, 0, 2, 0, "b");
        assert_eq!(rebase(&before, &replaced).unwrap(), before);
    }
}
