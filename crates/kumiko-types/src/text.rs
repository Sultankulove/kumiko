//! Text coordinates and edits.
//!
//! Positions are UTF-8 based (ADR-012): `character` is a byte offset inside the line.
//! Conversion to UTF-16 happens only at the LSP boundary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Position in a document: zero-based line and UTF-8 byte offset inside that line.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct Position {
    pub line: u32,
    /// UTF-8 byte offset inside the line.
    pub character: u32,
}

impl Position {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// Half-open range `[start, end)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn contains(&self, position: Position) -> bool {
        self.start <= position && position < self.end
    }
}

/// Replacement of `range` with `new_text`; an empty range inserts, empty text deletes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// Selection with a fixed `anchor` and a moving `head` (the caret).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
}

/// Monotonic document version; every transaction increments it by one.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct DocumentVersion(pub u64);

impl DocumentVersion {
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_order_by_line_then_character() {
        assert!(Position::new(1, 9) < Position::new(2, 0));
        assert!(Position::new(2, 0) < Position::new(2, 1));
    }

    #[test]
    fn range_is_half_open() {
        let range = Range::new(Position::new(0, 2), Position::new(0, 5));
        assert!(range.contains(Position::new(0, 2)));
        assert!(!range.contains(Position::new(0, 5)));
        assert!(Range::new(Position::new(3, 1), Position::new(3, 1)).is_empty());
    }

    #[test]
    fn text_edit_wire_format() {
        let edit = TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(0, 3)),
            new_text: "let".into(),
        };
        let json = serde_json::to_value(&edit).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
                "newText": "let"
            })
        );
        assert_eq!(serde_json::from_value::<TextEdit>(json).unwrap(), edit);
        assert_eq!(DocumentVersion(4).next(), DocumentVersion(5));
    }
}
