//! File bytes ↔ buffer text (spec, section 12): UTF-8 by default, BOMs, UTF-16 and Latin-1
//! on read; the original encoding and line endings are restored on save.

use crate::text::normalize_newlines;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Bytes that are not valid UTF-8 are read as ISO-8859-1.
    Latin1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

/// How a file is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextFormat {
    pub encoding: Encoding,
    pub line_ending: LineEnding,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            encoding: Encoding::Utf8,
            line_ending: LineEnding::Lf,
        }
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|&pair| {
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Decodes file bytes into normalized text (`\n` only) and the format to save them back.
pub fn decode(bytes: &[u8]) -> (String, TextFormat) {
    let (raw, encoding) = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        (
            String::from_utf8_lossy(rest).into_owned(),
            Encoding::Utf8Bom,
        )
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        (decode_utf16(rest, true), Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        (decode_utf16(rest, false), Encoding::Utf16Be)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => (text.to_owned(), Encoding::Utf8),
            Err(_) => (
                bytes.iter().map(|&b| char::from(b)).collect(),
                Encoding::Latin1,
            ),
        }
    };
    let crlf = raw.matches("\r\n").count();
    let lf = raw.matches('\n').count() - crlf;
    let line_ending = if crlf > lf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    };
    (
        normalize_newlines(&raw),
        TextFormat {
            encoding,
            line_ending,
        },
    )
}

/// Encodes buffer text for storage; `Err` with the first character Latin-1 cannot hold.
pub fn encode(text: &str, format: TextFormat) -> Result<Vec<u8>, char> {
    let text = match format.line_ending {
        LineEnding::Lf => std::borrow::Cow::Borrowed(text),
        LineEnding::CrLf => std::borrow::Cow::Owned(text.replace('\n', "\r\n")),
    };
    Ok(match format.encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => [b"\xEF\xBB\xBF".as_slice(), text.as_bytes()].concat(),
        Encoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            out
        }
        Encoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            out.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
            out
        }
        Encoding::Latin1 => {
            let mut out = Vec::with_capacity(text.len());
            for c in text.chars() {
                out.push(u8::try_from(u32::from(c)).map_err(|_| c)?);
            }
            out
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8]) -> (String, TextFormat) {
        let (text, format) = decode(bytes);
        assert_eq!(encode(&text, format).unwrap(), bytes, "{format:?}");
        (text, format)
    }

    #[test]
    fn encodings_round_trip() {
        let (text, format) = round_trip("héllo\nwörld\n".as_bytes());
        assert_eq!(
            (text.as_str(), format.encoding),
            ("héllo\nwörld\n", Encoding::Utf8)
        );

        let (text, format) = round_trip(b"\xEF\xBB\xBFbom\n");
        assert_eq!(
            (text.as_str(), format.encoding),
            ("bom\n", Encoding::Utf8Bom)
        );

        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("é\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let (text, format) = round_trip(&le);
        assert_eq!(text, "é\n");
        assert_eq!(
            format,
            TextFormat {
                encoding: Encoding::Utf16Le,
                line_ending: LineEnding::CrLf
            }
        );

        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("ok".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(round_trip(&be).1.encoding, Encoding::Utf16Be);

        let (text, format) = round_trip(b"caf\xE9\r\n");
        assert_eq!(
            (text.as_str(), format.encoding),
            ("café\n", Encoding::Latin1)
        );
    }

    #[test]
    fn dominant_line_ending_wins() {
        let (text, format) = decode(b"a\r\nb\r\nc\n");
        assert_eq!(text, "a\nb\nc\n");
        assert_eq!(format.line_ending, LineEnding::CrLf);
        assert_eq!(encode(&text, format).unwrap(), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn latin1_rejects_wide_characters() {
        let format = TextFormat {
            encoding: Encoding::Latin1,
            line_ending: LineEnding::Lf,
        };
        assert_eq!(encode("x€", format), Err('€'));
    }
}
