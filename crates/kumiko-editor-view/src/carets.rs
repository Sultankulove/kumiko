//! Working with several carets, as in JetBrains IDEs: clone the caret to the row above or
//! below, add the next occurrence of the selected text (Alt+J), drop it again (Alt+Shift+J),
//! select every occurrence; plus word, line and whole-text selection.

use ropey::Rope;

use crate::display::DisplayMap;
use crate::movement::{CharClass, class};
use crate::selection::{Range, Selection};
use crate::text::line_len;

/// The word (or run of punctuation) touching `at`; empty between blanks.
pub fn word_at(rope: &Rope, at: usize) -> (usize, usize) {
    let len = rope.len_chars();
    let at = at.min(len);
    // Prefer the word after the caret, then the one before it.
    let probe = if at < len && class(rope.char(at)) == CharClass::Word {
        at
    } else if at > 0 && class(rope.char(at - 1)) == CharClass::Word {
        at - 1
    } else if at < len && class(rope.char(at)) == CharClass::Punctuation {
        at
    } else {
        return (at, at);
    };
    let run = class(rope.char(probe));
    let mut start = probe;
    while start > 0 && class(rope.char(start - 1)) == run {
        start -= 1;
    }
    let mut end = probe;
    while end < len && class(rope.char(end)) == run {
        end += 1;
    }
    (start, end)
}

/// Double click: the word under the caret.
pub fn select_word(rope: &Rope, at: usize) -> Range {
    let (start, end) = word_at(rope, at);
    Range::new(start, end)
}

/// Triple click: the whole line with its break.
pub fn select_line(rope: &Rope, at: usize) -> Range {
    let line = rope.char_to_line(at.min(rope.len_chars()));
    let start = rope.line_to_char(line);
    let end = if line + 1 < rope.len_lines() {
        rope.line_to_char(line + 1)
    } else {
        start + line_len(rope, line)
    };
    Range::new(start, end)
}

pub fn select_all(rope: &Rope) -> Selection {
    Selection::single(Range::new(0, rope.len_chars()))
}

/// A new caret on the row above or below the primary one, at its column.
pub fn clone_caret(rope: &Rope, map: &DisplayMap, selection: &Selection, up: bool) -> Selection {
    // The caret furthest in the direction of the move clones itself.
    let edge = if up {
        selection.ranges().first()
    } else {
        selection.ranges().last()
    };
    let Some(edge) = edge else {
        return selection.clone();
    };
    let (row, column) = map.locate(rope, edge.head);
    let goal = edge.goal.unwrap_or(column);
    let target = if up {
        let Some(row) = row.checked_sub(1) else {
            return selection.clone();
        };
        row
    } else if row + 1 < map.row_count() {
        row + 1
    } else {
        return selection.clone();
    };
    let mut caret = Range::point(map.char_at(rope, target, goal));
    caret.goal = Some(goal);
    selection.with(caret)
}

/// Every occurrence of `needle` in the text, as char ranges.
fn occurrences(rope: &Rope, needle: &str, whole_word: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let text = rope.to_string();
    let needle_chars = needle.chars().count();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(index) = text[from..].find(needle) {
        let byte = from + index;
        let start = rope.byte_to_char(byte);
        let end = start + needle_chars;
        let bounded = !whole_word
            || ((start == 0 || class(rope.char(start - 1)) != CharClass::Word)
                && (end >= rope.len_chars() || class(rope.char(end)) != CharClass::Word));
        if bounded {
            found.push((start, end));
        }
        from = byte + needle.len().max(1);
    }
    found
}

/// Alt+J: with a caret, selects the word under it; with a selection, adds the next
/// occurrence of its text (wrapping around) as the new primary range.
pub fn select_next_occurrence(rope: &Rope, selection: &Selection) -> Selection {
    let primary = selection.primary();
    if primary.is_empty() {
        if selection.len() > 1 {
            return selection.clone();
        }
        return Selection::single(select_word(rope, primary.head));
    }
    let needle = rope.slice(primary.start()..primary.end()).to_string();
    // A selected whole word finds whole words only (not `foo` inside `food`).
    let whole_word = word_at(rope, primary.start()) == (primary.start(), primary.end());
    let all = occurrences(rope, &needle, whole_word);
    let taken = |r: &(usize, usize)| {
        selection
            .ranges()
            .iter()
            .any(|s| s.start() == r.0 && s.end() == r.1)
    };
    let after = selection.ranges().last().map_or(0, Range::end);
    let next = all
        .iter()
        .find(|r| r.0 >= after && !taken(r))
        .or_else(|| all.iter().find(|r| !taken(r)));
    match next {
        Some(&(start, end)) => selection.with(Range::new(start, end)),
        None => selection.clone(),
    }
}

/// Alt+Shift+J: drops the primary range (the last one added).
pub fn unselect_occurrence(selection: &Selection) -> Selection {
    if selection.len() < 2 {
        return selection.clone();
    }
    let mut ranges = selection.ranges().to_vec();
    let primary = selection.primary_index();
    ranges.remove(primary);
    let last = ranges.len() - 1;
    Selection::new(ranges, primary.min(last))
}

/// Ctrl+Alt+Shift+J: every occurrence of the primary selection (or of the word at the caret).
pub fn select_all_occurrences(rope: &Rope, selection: &Selection) -> Selection {
    let primary = selection.primary();
    let (range, whole_word) = if primary.is_empty() {
        (select_word(rope, primary.head), true)
    } else {
        (primary, false)
    };
    if range.is_empty() {
        return selection.clone();
    }
    let needle = rope.slice(range.start()..range.end()).to_string();
    let all: Vec<Range> = occurrences(rope, &needle, whole_word)
        .into_iter()
        .map(|(s, e)| Range::new(s, e))
        .collect();
    let primary = all
        .iter()
        .position(|r| r.start() == range.start())
        .unwrap_or(0);
    if all.is_empty() {
        selection.clone()
    } else {
        Selection::new(all, primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayConfig;

    #[test]
    fn words_lines_and_all() {
        let rope = Rope::from_str("let foo = bar;\nnext");
        assert_eq!(select_word(&rope, 5), Range::new(4, 7));
        assert_eq!(
            select_word(&rope, 7),
            Range::new(4, 7),
            "right after a word"
        );
        assert_eq!(select_word(&rope, 9), Range::new(9, 9), "between blanks");
        assert_eq!(select_line(&rope, 2), Range::new(0, 15));
        assert_eq!(select_line(&rope, 16), Range::new(15, 19));
        assert_eq!(select_all(&rope).primary(), Range::new(0, 19));
    }

    #[test]
    fn carets_clone_up_and_down_keeping_the_column() {
        let rope = Rope::from_str("abcdef\nab\nabcdef");
        let map = DisplayMap::new(&rope, DisplayConfig::default());
        let down = clone_caret(&rope, &map, &Selection::point(4), false);
        assert_eq!(down.len(), 2);
        assert_eq!(down.primary().head, 9, "end of the short line");
        let further = clone_caret(&rope, &map, &down, false);
        assert_eq!(further.primary().head, 14, "back at column 4");
        let top = clone_caret(&rope, &map, &Selection::point(1), true);
        assert_eq!(top.len(), 1, "nothing above the first row");
    }

    #[test]
    fn next_occurrence_like_alt_j() {
        let rope = Rope::from_str("foo food foo\nfoo");
        let word = select_next_occurrence(&rope, &Selection::point(1));
        assert_eq!(word.primary(), Range::new(0, 3));
        let second = select_next_occurrence(&rope, &word);
        assert_eq!(
            second.primary(),
            Range::new(9, 12),
            "whole words: not inside food"
        );
        let third = select_next_occurrence(&rope, &second);
        assert_eq!(third.primary(), Range::new(13, 16));
        let wrapped = select_next_occurrence(&rope, &third);
        assert_eq!(wrapped.len(), 3, "all taken");
        let back = unselect_occurrence(&third);
        assert_eq!(back.ranges(), [Range::new(0, 3), Range::new(9, 12)]);
        // A selection made by hand matches inside words too.
        let typed = Selection::single(Range::new(4, 7));
        let next = select_next_occurrence(&rope, &typed);
        assert_eq!(next.primary(), Range::new(9, 12));
        let all = select_all_occurrences(&rope, &Selection::point(14));
        assert_eq!(all.len(), 3);
        assert_eq!(all.primary(), Range::new(13, 16));
    }
}
