//! Full-text search over the files of the index (Find in Files, spec section 11): a literal or
//! a regular expression, case and whole word as options, line by line. Binary and very large
//! files are skipped; open documents are searched in their current (unsaved) text. Results
//! stream to a callback that can stop the search.

use std::collections::HashMap;
use std::ops::{ControlFlow, Range};

use kumiko_types::Uri;
use regex::{Regex, RegexBuilder};

use crate::IndexedFile;

/// Files larger than this are not searched.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// A shown line is cut to this many bytes around the match.
const MAX_LINE: usize = 400;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl TextQuery {
    /// The regular expression to run; `Err` with a message for an invalid pattern.
    pub fn compile(&self) -> Result<Regex, String> {
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
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    pub file: IndexedFile,
    /// 0-based line.
    pub line: u32,
    /// UTF-8 byte range of the match in the line (in `text`'s coordinates, see `text_start`).
    pub range: Range<usize>,
    /// The line, possibly cut around the match; `range` is relative to the full line.
    pub text: String,
    /// Byte offset of `text` in the full line.
    pub text_start: usize,
}

/// Searches `files`; `open` holds the text of open documents, used instead of the disk.
/// `found` gets each match and may stop the search.
pub fn search(
    files: &[IndexedFile],
    open: &HashMap<Uri, String>,
    query: &Regex,
    mut found: impl FnMut(TextMatch) -> ControlFlow<()>,
) {
    for file in files {
        let text = match open.get(&file.uri) {
            Some(text) => std::borrow::Cow::Borrowed(text.as_str()),
            None => match read(file) {
                Some(text) => std::borrow::Cow::Owned(text),
                None => continue,
            },
        };
        if search_text(file, &text, query, &mut found).is_break() {
            return;
        }
    }
}

/// The file's text, unless it is missing, too large or binary.
fn read(file: &IndexedFile) -> Option<String> {
    let path = file.uri.to_file_path()?;
    if std::fs::metadata(&path).ok()?.len() > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // A NUL in the first 8 KiB: binary.
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    })
}

fn search_text(
    file: &IndexedFile,
    text: &str,
    query: &Regex,
    found: &mut impl FnMut(TextMatch) -> ControlFlow<()>,
) -> ControlFlow<()> {
    // Line starts are computed lazily, only for files that match.
    let mut starts: Option<Vec<usize>> = None;
    for m in query.find_iter(text) {
        if m.start() == m.end() {
            continue; // empty matches (`^`, `a*`) say nothing
        }
        let starts = starts.get_or_insert_with(|| {
            std::iter::once(0)
                .chain(text.match_indices('\n').map(|(i, _)| i + 1))
                .collect()
        });
        let line = starts.partition_point(|&s| s <= m.start()) - 1;
        let line_start = starts[line];
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |i| line_start + i);
        let line_text = text[line_start..line_end].trim_end_matches('\r');
        let range = m.start() - line_start..(m.end().min(line_end)) - line_start;
        let (shown, shown_start) = cut(line_text, &range);
        found(TextMatch {
            file: file.clone(),
            line: line as u32,
            range,
            text: shown.to_owned(),
            text_start: shown_start,
        })?;
    }
    ControlFlow::Continue(())
}

/// At most [`MAX_LINE`] bytes of the line around the match, on char boundaries.
fn cut<'a>(line: &'a str, range: &Range<usize>) -> (&'a str, usize) {
    if line.len() <= MAX_LINE {
        return (line, 0);
    }
    let mut start = range.start.saturating_sub(MAX_LINE / 4).min(line.len());
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + MAX_LINE).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (&line[start..end], start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &std::path::Path, name: &str, text: &[u8]) -> IndexedFile {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        IndexedFile {
            uri: Uri::from_file_path(&path).unwrap(),
            relative: name.into(),
            folder: "w".into(),
        }
    }

    fn all(
        files: &[IndexedFile],
        open: &HashMap<Uri, String>,
        query: TextQuery,
    ) -> Vec<(String, u32, String)> {
        let regex = query.compile().unwrap();
        let mut out = Vec::new();
        search(files, open, &regex, |m| {
            let shown = &m.text[m.range.start - m.text_start..m.range.end - m.text_start];
            out.push((m.file.relative.clone(), m.line, shown.to_owned()));
            ControlFlow::Continue(())
        });
        out
    }

    #[test]
    fn finds_lines_with_options_and_skips_binaries() {
        let temp = tempfile::tempdir().unwrap();
        let files = vec![
            file(
                temp.path(),
                "a.txt",
                "Hello world\r\nhello again\nyellow\n".as_bytes(),
            ),
            file(temp.path(), "b.bin", b"hello\0binary"),
            file(temp.path(), "c.txt", "привет hello".as_bytes()),
        ];
        let open = HashMap::new();
        let query = |text: &str| TextQuery {
            text: text.into(),
            ..TextQuery::default()
        };
        assert_eq!(
            all(&files, &open, query("hello")),
            [
                ("a.txt".into(), 0, "Hello".into()),
                ("a.txt".into(), 1, "hello".into()),
                ("c.txt".into(), 0, "hello".into()),
            ]
        );
        let sensitive = TextQuery {
            case_sensitive: true,
            ..query("Hello")
        };
        assert_eq!(all(&files, &open, sensitive).len(), 1);
        let word = TextQuery {
            whole_word: true,
            ..query("ello")
        };
        assert!(
            all(&files, &open, word).is_empty(),
            "yellow, hello are other words"
        );
        let regex = TextQuery {
            regex: true,
            ..query(r"l+o\b")
        };
        assert_eq!(
            all(&files, &open, regex).len(),
            3,
            "not in yellow: no word end"
        );
        assert!(
            TextQuery {
                regex: true,
                ..query("(")
            }
            .compile()
            .is_err()
        );
    }

    #[test]
    fn open_documents_win_and_the_callback_stops_the_search() {
        let temp = tempfile::tempdir().unwrap();
        let files = vec![
            file(temp.path(), "a.txt", b"on disk\n"),
            file(temp.path(), "b.txt", b"needle\nneedle\n"),
        ];
        let mut open = HashMap::new();
        open.insert(files[0].uri.clone(), "unsaved needle\n".to_owned());
        let regex = TextQuery {
            text: "needle".into(),
            ..TextQuery::default()
        }
        .compile()
        .unwrap();
        let mut seen = Vec::new();
        search(&files, &open, &regex, |m| {
            seen.push(m.file.relative.clone());
            if seen.len() == 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(seen, ["a.txt", "b.txt"]);
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let line = format!("{}needle{}", "é".repeat(500), "x".repeat(500));
        let start = line.find("needle").unwrap();
        let (shown, at) = cut(&line, &(start..start + 6));
        assert!(shown.len() <= MAX_LINE);
        assert!(shown.contains("needle"));
        assert_eq!(&line[at..at + shown.len()], shown);
    }
}
