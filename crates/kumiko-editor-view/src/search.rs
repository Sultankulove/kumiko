//! Find and replace in a file (spec, section 12): plain text or a regular expression, with
//! case and whole-word options; replacing one match or all of them in one transaction.

use regex::{Regex, RegexBuilder};
use ropey::Rope;

use crate::editing::Edit;
use crate::selection::{Change, Range, Selection};

/// More matches than this are not collected (the count says "10000+").
pub const MAX_MATCHES: usize = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl Query {
    /// The pattern to search with; an invalid regular expression is an error to show.
    pub fn compile(&self) -> Result<Option<Regex>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let mut pattern = if self.regex {
            self.text.clone()
        } else {
            regex::escape(&self.text)
        };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .multi_line(true)
            .build()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

/// Matches as char ranges, in order; empty matches are skipped.
pub fn find_all(rope: &Rope, regex: &Regex) -> Vec<(usize, usize)> {
    let text = rope.to_string();
    regex
        .find_iter(&text)
        .filter(|m| !m.is_empty())
        .take(MAX_MATCHES)
        .map(|m| (rope.byte_to_char(m.start()), rope.byte_to_char(m.end())))
        .collect()
}

/// The match to go to from `at`: the first starting after it (or before it, backwards),
/// wrapping around.
pub fn next_match(matches: &[(usize, usize)], at: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let found = if forward {
        matches.iter().position(|m| m.0 > at)
    } else {
        matches.iter().rposition(|m| m.0 < at)
    };
    Some(found.unwrap_or(if forward { 0 } else { matches.len() - 1 }))
}

/// The replacement for a match found in `text` at byte `start`: `$1`-style groups expand in
/// regex mode (captured in the whole text, so anchors and `\b` behave), plain text otherwise.
fn replacement(text: &str, regex: &Regex, start: usize, with: &str, regex_mode: bool) -> String {
    if !regex_mode {
        return with.to_owned();
    }
    match regex.captures_at(text, start) {
        Some(captures) => {
            let mut out = String::new();
            captures.expand(with, &mut out);
            out
        }
        None => with.to_owned(),
    }
}

/// Replaces one match; the caret ends after the new text.
pub fn replace_one(
    rope: &Rope,
    query: &Query,
    regex: &Regex,
    at: (usize, usize),
    with: &str,
) -> Edit {
    let text = rope.to_string();
    let new_text = replacement(&text, regex, rope.char_to_byte(at.0), with, query.regex);
    let changes = vec![Change {
        start: at.0,
        end: at.1,
        text: new_text,
    }];
    Edit {
        selection: Selection::single(Range::new(at.0, at.1)).after(&changes),
        changes,
    }
}

/// Replaces every match in one transaction; the caret ends after the last replacement.
pub fn replace_all(rope: &Rope, query: &Query, regex: &Regex, with: &str) -> Edit {
    let text = rope.to_string();
    let changes: Vec<Change> = regex
        .find_iter(&text)
        .filter(|m| !m.is_empty())
        .take(MAX_MATCHES)
        .map(|m| Change {
            start: rope.byte_to_char(m.start()),
            end: rope.byte_to_char(m.end()),
            text: replacement(&text, regex, m.start(), with, query.regex),
        })
        .collect();
    let selection = match changes.last() {
        Some(last) => Selection::single(Range::new(last.start, last.end)).after(&changes),
        None => Selection::point(0),
    };
    Edit { changes, selection }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Query {
        Query {
            text: text.into(),
            ..Query::default()
        }
    }

    fn apply(rope: &mut Rope, edit: &Edit) {
        for change in edit.changes.iter().rev() {
            rope.remove(change.start..change.end);
            rope.insert(change.start, &change.text);
        }
    }

    #[test]
    fn options_shape_the_search() {
        let rope = Rope::from_str("Foo food foo.Bar");
        let all = |q: Query| find_all(&rope, &q.compile().unwrap().unwrap());
        assert_eq!(all(query("foo")), [(0, 3), (4, 7), (9, 12)], "any case");
        let cased = Query {
            case_sensitive: true,
            ..query("foo")
        };
        assert_eq!(all(cased), [(4, 7), (9, 12)]);
        let words = Query {
            whole_word: true,
            ..query("foo")
        };
        assert_eq!(all(words), [(0, 3), (9, 12)]);
        let regex = Query {
            regex: true,
            ..query(r"\.(\w+)")
        };
        assert_eq!(all(regex), [(12, 16)]);
        assert!(matches!(query("").compile(), Ok(None)));
        let broken = Query {
            regex: true,
            ..query("(")
        };
        assert!(broken.compile().is_err());
        assert_eq!(
            all(query("a.b")),
            Vec::<(usize, usize)>::new(),
            "plain text is literal"
        );
    }

    #[test]
    fn going_to_the_next_match_wraps() {
        let matches = [(2, 3), (5, 6), (9, 10)];
        assert_eq!(next_match(&matches, 0, true), Some(0));
        assert_eq!(next_match(&matches, 5, true), Some(2));
        assert_eq!(next_match(&matches, 9, true), Some(0), "wraps");
        assert_eq!(next_match(&matches, 5, false), Some(0));
        assert_eq!(next_match(&matches, 1, false), Some(2), "wraps backwards");
        assert_eq!(next_match(&[], 1, true), None);
    }

    #[test]
    fn replacing_one_and_all() {
        let mut rope = Rope::from_str("a1 b22 c333");
        let q = Query {
            regex: true,
            ..query(r"([a-z])(\d+)")
        };
        let regex = q.compile().unwrap().unwrap();
        let one = replace_one(&rope, &q, &regex, (0, 2), "$2$1");
        apply(&mut rope, &one);
        assert_eq!(rope.to_string(), "1a b22 c333");
        assert_eq!(one.selection.primary(), Range::point(2));
        let all = replace_all(&rope, &q, &regex, "<$1>");
        apply(&mut rope, &all);
        assert_eq!(
            rope.to_string(),
            "1a <b> <c>",
            "1a has no letter before digits"
        );
        let plain = query("$");
        let mut text = Rope::from_str("$x$");
        let regex = plain.compile().unwrap().unwrap();
        let edit = replace_all(&text, &plain, &regex, "$1");
        apply(&mut text, &edit);
        assert_eq!(text.to_string(), "$1x$1", "no expansion for plain text");
    }
}
