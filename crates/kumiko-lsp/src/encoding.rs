//! Positions inside the IDE are UTF-8 bytes within a line (ADR-012); a server speaks UTF-8 if
//! it agreed to, UTF-16 otherwise. Conversion needs the line's text, so it happens against the
//! client's copy of the document at the version the server knows.

use ropey::Rope;

use kumiko_types::{Position, Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    /// The encoding the server chose in `initialize` (absent means UTF-16).
    pub fn negotiated(capabilities: &lsp_types::ServerCapabilities) -> Self {
        match &capabilities.position_encoding {
            Some(kind) if *kind == lsp_types::PositionEncodingKind::UTF8 => Self::Utf8,
            _ => Self::Utf16,
        }
    }
}

/// Text of a line without its line break.
fn line_text(rope: &Rope, line: usize) -> Option<ropey::RopeSlice<'_>> {
    (line < rope.len_lines()).then(|| {
        let slice = rope.line(line);
        let mut end = slice.len_chars();
        while end > 0 && matches!(slice.char(end - 1), '\n' | '\r') {
            end -= 1;
        }
        slice.slice(..end)
    })
}

pub fn to_lsp(rope: &Rope, encoding: Encoding, position: Position) -> lsp_types::Position {
    let character = match (encoding, line_text(rope, position.line as usize)) {
        (Encoding::Utf8, _) | (_, None) => position.character,
        (Encoding::Utf16, Some(line)) => {
            let byte = (position.character as usize).min(line.len_bytes());
            let chars = line.byte_to_char(byte);
            line.char_to_utf16_cu(chars) as u32
        }
    };
    lsp_types::Position::new(position.line, character)
}

/// A server position, clamped into the text: servers report positions past line ends.
pub fn from_lsp(rope: &Rope, encoding: Encoding, position: lsp_types::Position) -> Position {
    let Some(line) = line_text(rope, position.line as usize) else {
        // Past the end: the end of the text.
        let last = rope.len_lines().saturating_sub(1);
        let len = line_text(rope, last).map_or(0, |l| l.len_bytes());
        return Position::new(last as u32, len as u32);
    };
    let byte = match encoding {
        Encoding::Utf8 => {
            let byte = (position.character as usize).min(line.len_bytes());
            // Onto a char boundary.
            line.char_to_byte(line.byte_to_char(byte))
        }
        Encoding::Utf16 => {
            let units = (position.character as usize).min(line.len_utf16_cu());
            line.char_to_byte(line.utf16_cu_to_char(units))
        }
    };
    Position::new(position.line, byte as u32)
}

pub fn range_to_lsp(rope: &Rope, encoding: Encoding, range: Range) -> lsp_types::Range {
    lsp_types::Range::new(
        to_lsp(rope, encoding, range.start),
        to_lsp(rope, encoding, range.end),
    )
}

pub fn range_from_lsp(rope: &Rope, encoding: Encoding, range: lsp_types::Range) -> Range {
    Range::new(
        from_lsp(rope, encoding, range.start),
        from_lsp(rope, encoding, range.end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_counts_units_and_utf8_passes_through() {
        // "п" is 2 bytes and 1 unit, "😀" is 4 bytes and 2 units.
        let rope = Rope::from_str("a\nп😀x\r\n");
        let x = Position::new(1, 6);
        assert_eq!(
            to_lsp(&rope, Encoding::Utf16, x),
            lsp_types::Position::new(1, 3)
        );
        assert_eq!(
            to_lsp(&rope, Encoding::Utf8, x),
            lsp_types::Position::new(1, 6)
        );
        assert_eq!(
            from_lsp(&rope, Encoding::Utf16, lsp_types::Position::new(1, 3)),
            x
        );
        assert_eq!(
            from_lsp(&rope, Encoding::Utf8, lsp_types::Position::new(1, 6)),
            x
        );
    }

    #[test]
    fn server_positions_are_clamped() {
        let rope = Rope::from_str("ab\r\ncd");
        assert_eq!(
            from_lsp(&rope, Encoding::Utf16, lsp_types::Position::new(0, 99)),
            Position::new(0, 2),
            "not into the line break"
        );
        assert_eq!(
            from_lsp(&rope, Encoding::Utf16, lsp_types::Position::new(7, 0)),
            Position::new(1, 2)
        );
        // Inside a multi-byte char: back to its start.
        let rope = Rope::from_str("пр");
        assert_eq!(
            from_lsp(&rope, Encoding::Utf8, lsp_types::Position::new(0, 3)),
            Position::new(0, 2)
        );
    }
}
