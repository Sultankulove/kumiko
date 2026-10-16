//! Editing commands at every caret: each returns the transaction (changes against the
//! current text) and the selection after it. Behaviour follows JetBrains IDEs: copy or cut
//! without a selection takes the whole line, Ctrl+D duplicates, Ctrl+Y deletes lines,
//! Enter keeps the indentation.

use kumiko_types::TextEdit;
use ropey::Rope;

use crate::movement::{CharClass, class};
use crate::selection::{Assoc, Change, Range, Selection, map_position};
use crate::text::{line_len, next_grapheme, position_of, prev_grapheme};

/// A transaction and where the carets end up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub changes: Vec<Change>,
    pub selection: Selection,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The changes as protocol edits against the text they apply to.
    pub fn text_edits(&self, rope: &Rope) -> Vec<TextEdit> {
        self.changes
            .iter()
            .map(|c| TextEdit {
                range: kumiko_types::Range::new(
                    position_of(rope, c.start),
                    position_of(rope, c.end),
                ),
                new_text: c.text.clone(),
            })
            .collect()
    }
}

/// Indentation settings of the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indent {
    pub width: usize,
    pub tabs: bool,
}

impl Default for Indent {
    fn default() -> Self {
        Self {
            width: 4,
            tabs: false,
        }
    }
}

impl Indent {
    fn unit(self) -> String {
        if self.tabs {
            "\t".to_owned()
        } else {
            " ".repeat(self.width.max(1))
        }
    }
}

/// Sorted, non-overlapping changes (overlaps from merged ranges are dropped).
fn changes(mut changes: Vec<Change>) -> Vec<Change> {
    changes.sort_by_key(|c| (c.start, c.end));
    let mut out: Vec<Change> = Vec::with_capacity(changes.len());
    for change in changes {
        if out.last().is_some_and(|last| change.start < last.end) {
            continue;
        }
        out.push(change);
    }
    out
}

/// Replaces every range with `text`.
pub fn insert(selection: &Selection, text: &str) -> Edit {
    let changes = changes(
        selection
            .ranges()
            .iter()
            .map(|r| Change {
                start: r.start(),
                end: r.end(),
                text: text.to_owned(),
            })
            .collect(),
    );
    Edit {
        selection: selection.after(&changes),
        changes,
    }
}

/// Deletes selections, or what `step` finds next to empty carets.
fn delete_with(rope: &Rope, selection: &Selection, step: impl Fn(&Rope, usize) -> usize) -> Edit {
    let changes = changes(
        selection
            .ranges()
            .iter()
            .filter_map(|r| {
                let (start, end) = if r.is_empty() {
                    let other = step(rope, r.head);
                    (other.min(r.head), other.max(r.head))
                } else {
                    (r.start(), r.end())
                };
                (start < end).then(|| Change::delete(start, end))
            })
            .collect(),
    );
    Edit {
        selection: selection.after(&changes),
        changes,
    }
}

pub fn backspace(rope: &Rope, selection: &Selection) -> Edit {
    delete_with(rope, selection, prev_grapheme)
}

pub fn delete(rope: &Rope, selection: &Selection) -> Edit {
    delete_with(rope, selection, next_grapheme)
}

/// Ctrl+Backspace: back to the start of the word (or blanks) before the caret.
pub fn delete_word_left(rope: &Rope, selection: &Selection) -> Edit {
    delete_with(rope, selection, |rope, at| {
        let mut pos = at;
        if pos > 0 && rope.char(pos - 1) == '\n' {
            return pos - 1;
        }
        let run = if pos > 0 {
            class(rope.char(pos - 1))
        } else {
            CharClass::Space
        };
        while pos > 0 && class(rope.char(pos - 1)) == run && run != CharClass::Newline {
            pos -= 1;
        }
        pos
    })
}

/// Ctrl+Delete: up to the end of the word (or blanks) after the caret.
pub fn delete_word_right(rope: &Rope, selection: &Selection) -> Edit {
    delete_with(rope, selection, |rope, at| {
        let len = rope.len_chars();
        let mut pos = at;
        if pos < len && rope.char(pos) == '\n' {
            return pos + 1;
        }
        let run = if pos < len {
            class(rope.char(pos))
        } else {
            CharClass::Space
        };
        while pos < len && class(rope.char(pos)) == run && run != CharClass::Newline {
            pos += 1;
        }
        pos
    })
}

/// Leading blanks of the line holding `char`.
fn leading_blanks(rope: &Rope, line: usize) -> String {
    rope.line(line)
        .chars()
        .take(line_len(rope, line))
        .take_while(|c| matches!(c, ' ' | '\t'))
        .collect()
}

/// Enter: a new line with the indentation of the current one, one level deeper after an
/// opening bracket; between brackets (`{|}`) the closing one moves to a line of its own.
pub fn newline(rope: &Rope, selection: &Selection, indent: Indent) -> Edit {
    let mut changes_list = Vec::new();
    let mut carets = Vec::new();
    for range in selection.ranges() {
        let line = rope.char_to_line(range.start());
        let base = leading_blanks(rope, line);
        let before = (range.start() > 0).then(|| rope.char(range.start() - 1));
        let after = (range.end() < rope.len_chars()).then(|| rope.char(range.end()));
        let opens = matches!(before, Some('{' | '(' | '['));
        let closes = matches!(
            (before, after),
            (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
        );
        let inner = if opens {
            format!("{base}{}", indent.unit())
        } else {
            base.clone()
        };
        let text = if closes {
            format!("\n{inner}\n{base}")
        } else {
            format!("\n{inner}")
        };
        // The caret goes after the first line break and the inner indentation.
        carets.push(1 + inner.chars().count());
        changes_list.push(Change {
            start: range.start(),
            end: range.end(),
            text,
        });
    }
    let sorted = changes(changes_list.clone());
    let selection = Selection::new(
        selection
            .ranges()
            .iter()
            .zip(&carets)
            .map(|(range, offset)| {
                let start = map_position(range.start(), &sorted, Assoc::Before);
                Range::point(start + offset)
            })
            .collect(),
        selection.primary_index(),
    );
    Edit {
        changes: sorted,
        selection,
    }
}

/// The lines touched by the ranges, each once, ascending.
fn lines_of(rope: &Rope, selection: &Selection) -> Vec<usize> {
    let mut lines = Vec::new();
    for range in selection.ranges() {
        let first = rope.char_to_line(range.start());
        let mut last = rope.char_to_line(range.end());
        // A selection ending at a line start does not take that line.
        if last > first && range.end() == rope.line_to_char(last) {
            last -= 1;
        }
        lines.extend(first..=last);
    }
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Tab: with carets only, inserts indentation up to the next stop; with selected text,
/// indents the selected lines.
pub fn indent(rope: &Rope, selection: &Selection, indent: Indent) -> Edit {
    if selection.ranges().iter().all(Range::is_empty) {
        let unit = if indent.tabs {
            "\t".to_owned()
        } else {
            let column = |r: &Range| crate::text::column_of(rope, r.head, indent.width);
            // One caret decides the width; carets at other columns get the same text.
            let at = column(&selection.primary());
            " ".repeat(indent.width.max(1) - at % indent.width.max(1))
        };
        return insert(selection, &unit);
    }
    let unit = indent.unit();
    let edits: Vec<Change> = lines_of(rope, selection)
        .into_iter()
        .filter(|&line| line_len(rope, line) > 0)
        .map(|line| Change::insert(rope.line_to_char(line), unit.clone()))
        .collect();
    let edits = changes(edits);
    let selection = selection.transform(|r| Range {
        anchor: map_position(r.anchor, &edits, Assoc::After),
        head: map_position(r.head, &edits, Assoc::After),
        goal: None,
    });
    Edit {
        changes: edits,
        selection,
    }
}

/// Shift+Tab: removes one level of indentation from the lines of the selection.
pub fn outdent(rope: &Rope, selection: &Selection, indent: Indent) -> Edit {
    let edits: Vec<Change> = lines_of(rope, selection)
        .into_iter()
        .filter_map(|line| {
            let start = rope.line_to_char(line);
            let blanks = leading_blanks(rope, line);
            let mut width = 0;
            let mut count = 0;
            for c in blanks.chars() {
                if width >= indent.width.max(1) {
                    break;
                }
                width += if c == '\t' { indent.width.max(1) } else { 1 };
                count += 1;
            }
            (count > 0).then(|| Change::delete(start, start + count))
        })
        .collect();
    let edits = changes(edits);
    Edit {
        selection: selection.map(&edits),
        changes: edits,
    }
}

/// Ctrl+D: a caret duplicates its line below; a selection duplicates itself after itself.
pub fn duplicate(rope: &Rope, selection: &Selection) -> Edit {
    let mut list = Vec::new();
    for range in selection.ranges() {
        if range.is_empty() {
            let line = rope.char_to_line(range.head);
            let start = rope.line_to_char(line);
            let end = start + line_len(rope, line);
            let text: String = rope.slice(start..end).to_string();
            list.push(Change::insert(end, format!("\n{text}")));
        } else {
            let text = rope.slice(range.start()..range.end()).to_string();
            list.push(Change::insert(range.end(), text));
        }
    }
    let edits = changes(list);
    // Carets follow the copy: a line caret moves one line down at the same column.
    let selection = selection.transform(|r| {
        if r.is_empty() {
            let line = rope.char_to_line(r.head);
            let column = r.head - rope.line_to_char(line);
            let new_start = map_position(rope.line_to_char(line), &edits, Assoc::Before);
            Range::point(new_start + line_len(rope, line) + 1 + column)
        } else {
            let start = map_position(r.end(), &edits, Assoc::Before);
            let len = r.end() - r.start();
            // The copy, inserted right after the original, is selected.
            Range::new(start, start + len)
        }
    });
    Edit {
        changes: edits,
        selection,
    }
}

/// Ctrl+Y: deletes the lines of the selection.
pub fn delete_lines(rope: &Rope, selection: &Selection) -> Edit {
    let lines = lines_of(rope, selection);
    let mut list = Vec::new();
    for &line in &lines {
        let start = rope.line_to_char(line);
        let next = if line + 1 < rope.len_lines() {
            rope.line_to_char(line + 1)
        } else {
            rope.len_chars()
        };
        // The last line takes the break before it instead.
        let (start, end) = if next == rope.len_chars() && line > 0 && line + 1 >= rope.len_lines() {
            (start - 1, next)
        } else {
            (start, next)
        };
        list.push(Change::delete(start, end));
    }
    let edits = changes(list);
    Edit {
        selection: selection
            .transform(|r| Range::point(map_position(r.head, &edits, Assoc::Before)))
            .clamp(rope.len_chars()),
        changes: edits,
    }
}

/// Alt+Shift+Up/Down: moves the lines of the selection over the neighbouring line.
pub fn move_lines(rope: &Rope, selection: &Selection, up: bool) -> Edit {
    let lines = lines_of(rope, selection);
    let (Some(&first), Some(&last)) = (lines.first(), lines.last()) else {
        return Edit {
            changes: Vec::new(),
            selection: selection.clone(),
        };
    };
    let last_line = crate::text::last_line(rope);
    if (up && first == 0) || (!up && last >= last_line) {
        return Edit {
            changes: Vec::new(),
            selection: selection.clone(),
        };
    }
    let text_of = |from: usize, to: usize| -> String {
        let start = rope.line_to_char(from);
        let end = rope.line_to_char(to) + line_len(rope, to);
        rope.slice(start..end).to_string()
    };
    let block = text_of(first, last);
    let (start, end, new_text, shift): (usize, usize, String, isize) = if up {
        let other = first - 1;
        let neighbour = text_of(other, other);
        let start = rope.line_to_char(other);
        let end = rope.line_to_char(last) + line_len(rope, last);
        let shift = -((neighbour.chars().count() + 1) as isize);
        (other, start, end, format!("{block}\n{neighbour}"), shift)
    } else {
        let other = last + 1;
        let neighbour = text_of(other, other);
        let start = rope.line_to_char(first);
        let end = rope.line_to_char(other) + line_len(rope, other);
        let shift = (neighbour.chars().count() + 1) as isize;
        (other, start, end, format!("{neighbour}\n{block}"), shift)
    };
    let moved = |c: usize| (c as isize + shift).max(0) as usize;
    Edit {
        changes: vec![Change {
            start,
            end,
            text: new_text,
        }],
        selection: selection.transform(|r| Range {
            anchor: moved(r.anchor),
            head: moved(r.head),
            goal: r.goal,
        }),
    }
}

/// What a copy takes: the selected texts, or whole lines for empty carets (as IntelliJ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Joined with line breaks: what goes to the system clipboard.
    pub text: String,
    /// One per caret, to paste back one per caret.
    pub parts: Vec<String>,
    /// Whole lines were copied: pasting inserts them above the caret's line.
    pub lines: bool,
}

pub fn copy(rope: &Rope, selection: &Selection) -> Copied {
    let lines = selection.ranges().iter().all(Range::is_empty);
    let parts: Vec<String> = selection
        .ranges()
        .iter()
        .map(|r| {
            if lines {
                let line = rope.char_to_line(r.head);
                let start = rope.line_to_char(line);
                format!("{}\n", rope.slice(start..start + line_len(rope, line)))
            } else {
                rope.slice(r.start()..r.end()).to_string()
            }
        })
        .collect();
    let text = if lines {
        parts.concat()
    } else {
        parts.join("\n")
    };
    Copied { text, parts, lines }
}

/// Ctrl+X: copies, then deletes the selected text or, for carets, their lines.
pub fn cut(rope: &Rope, selection: &Selection) -> (Copied, Edit) {
    let copied = copy(rope, selection);
    let edit = if copied.lines {
        delete_lines(rope, selection)
    } else {
        delete(rope, selection)
    };
    (copied, edit)
}

/// Ctrl+V: as many parts as carets go one per caret; copied lines go above the caret's
/// line; anything else is typed at every caret.
pub fn paste(rope: &Rope, selection: &Selection, clipboard: &str, copied: Option<&Copied>) -> Edit {
    let matching = copied.filter(|c| c.text == clipboard);
    if let Some(copied) = matching {
        if copied.lines && selection.ranges().iter().all(Range::is_empty) {
            let text = copied.text.clone();
            let edits = changes(
                selection
                    .ranges()
                    .iter()
                    .map(|r| {
                        Change::insert(rope.line_to_char(rope.char_to_line(r.head)), text.clone())
                    })
                    .collect(),
            );
            return Edit {
                // Carets stay where they were in their (now lower) lines.
                selection: selection
                    .transform(|r| Range::point(map_position(r.head, &edits, Assoc::After))),
                changes: edits,
            };
        }
        if copied.parts.len() == selection.len() && selection.len() > 1 {
            let list: Vec<Change> = selection
                .ranges()
                .iter()
                .zip(&copied.parts)
                .map(|(r, part)| Change {
                    start: r.start(),
                    end: r.end(),
                    text: part.clone(),
                })
                .collect();
            let edits = changes(list);
            return Edit {
                selection: selection.after(&edits),
                changes: edits,
            };
        }
    }
    insert(selection, clipboard)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies an edit and renders carets as `|` (and selections as `[...]`).
    fn run(text: &str, selection: Selection, f: impl Fn(&Rope, &Selection) -> Edit) -> String {
        let mut rope = Rope::from_str(text);
        let edit = f(&rope, &selection);
        for change in edit.changes.iter().rev() {
            rope.remove(change.start..change.end);
            rope.insert(change.start, &change.text);
        }
        let mut out = rope.to_string();
        let mut marks: Vec<(usize, &str)> = Vec::new();
        for r in edit.selection.ranges() {
            if r.is_empty() {
                marks.push((r.head, "|"));
            } else {
                marks.push((r.start(), "["));
                marks.push((r.end(), "]"));
            }
        }
        marks.sort_by_key(|m| std::cmp::Reverse(m.0));
        for (at, mark) in marks {
            let byte = Rope::from_str(&out).char_to_byte(at);
            out.insert_str(byte, mark);
        }
        out
    }

    fn carets(at: &[usize]) -> Selection {
        Selection::new(at.iter().map(|&a| Range::point(a)).collect(), 0)
    }

    #[test]
    fn typing_and_deleting_at_several_carets() {
        assert_eq!(
            run("ab\ncd", carets(&[1, 4]), |_, s| insert(s, "X")),
            "aX|b\ncX|d"
        );
        assert_eq!(run("ab\ncd", carets(&[1, 4]), backspace), "|b\n|d");
        assert_eq!(run("ab\ncd", carets(&[1, 4]), delete), "a|\nc|");
        let selected = Selection::single(Range::new(1, 4));
        assert_eq!(run("ab\ncd", selected, |_, s| insert(s, "-")), "a-|d");
        assert_eq!(
            run("é\u{301}x", carets(&[2]), backspace),
            "|x",
            "a whole grapheme"
        );
    }

    #[test]
    fn word_deletion() {
        assert_eq!(
            run("foo bar.baz", carets(&[7]), delete_word_left),
            "foo |.baz"
        );
        assert_eq!(
            run("foo bar.baz", carets(&[4]), delete_word_right),
            "foo |.baz"
        );
        assert_eq!(run("a\nb", carets(&[2]), delete_word_left), "a|b");
    }

    #[test]
    fn enter_keeps_and_deepens_indentation() {
        let indent = Indent::default();
        assert_eq!(
            run("    x", carets(&[5]), |r, s| newline(r, s, indent)),
            "    x\n    |"
        );
        assert_eq!(
            run("  if {}", carets(&[6]), |r, s| newline(r, s, indent)),
            "  if {\n      |\n  }"
        );
        assert_eq!(
            run("f(", carets(&[2]), |r, s| newline(r, s, indent)),
            "f(\n    |"
        );
    }

    #[test]
    fn tab_inserts_to_the_next_stop_or_indents_lines() {
        let indent = Indent::default();
        assert_eq!(
            run("ab", carets(&[1]), |r, s| self::indent(r, s, indent)),
            "a   |b"
        );
        let lines = Selection::single(Range::new(0, 4));
        assert_eq!(
            run("ab\ncd\nef", lines, |r, s| self::indent(r, s, indent)),
            "    [ab\n    c]d\nef"
        );
        assert_eq!(
            run(
                "      ab\n\tcd",
                Selection::single(Range::new(0, 12)),
                |r, s| outdent(r, s, indent)
            ),
            "[  ab\ncd]"
        );
    }

    #[test]
    fn duplicate_and_delete_lines() {
        assert_eq!(run("ab\ncd", carets(&[1]), duplicate), "ab\na|b\ncd");
        assert_eq!(
            run("abc", Selection::single(Range::new(0, 2)), duplicate),
            "ab[ab]c"
        );
        assert_eq!(run("ab\ncd\nef", carets(&[4]), delete_lines), "ab\n|ef");
        assert_eq!(run("ab\ncd", carets(&[4]), delete_lines), "ab|");
    }

    #[test]
    fn lines_move_up_and_down() {
        assert_eq!(
            run("a\nb\nc", carets(&[2]), |r, s| move_lines(r, s, true)),
            "|b\na\nc"
        );
        assert_eq!(
            run("a\nb\nc", carets(&[2]), |r, s| move_lines(r, s, false)),
            "a\nc\n|b"
        );
        assert_eq!(
            run("a\nb", carets(&[0]), |r, s| move_lines(r, s, true)),
            "|a\nb"
        );
    }

    #[test]
    fn copy_cut_and_paste_like_intellij() {
        let rope = Rope::from_str("one\ntwo");
        let line = copy(&rope, &carets(&[1]));
        assert_eq!((line.text.as_str(), line.lines), ("one\n", true));
        assert_eq!(
            run("x\ny", carets(&[3]), |r, s| paste(
                r,
                s,
                "one\n",
                Some(&line)
            )),
            "x\none\ny|"
        );
        let parts = copy(
            &rope,
            &Selection::new(vec![Range::new(0, 1), Range::new(4, 5)], 0),
        );
        assert_eq!(parts.text, "o\nt");
        assert_eq!(
            run("ab", carets(&[0, 2]), |r, s| paste(
                r,
                s,
                "o\nt",
                Some(&parts)
            )),
            "o|abt|"
        );
        assert_eq!(
            run("ab", carets(&[1]), |r, s| paste(r, s, "Z", None)),
            "aZ|b"
        );
        let (copied, _) = cut(&rope, &carets(&[5]));
        assert_eq!(copied.text, "two\n");
        assert_eq!(run("one\ntwo", carets(&[5]), |r, s| cut(r, s).1), "one|");
    }
}
