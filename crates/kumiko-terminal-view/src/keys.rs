//! Keys as the bytes a terminal sends (xterm): text as UTF-8, control keys as C0 codes,
//! cursor and function keys as escape sequences (application mode for the arrows when the
//! program asks for it).

/// What a key sends; `None` for keys the terminal leaves to the IDE.
pub fn bytes(
    key: &str,
    typed: Option<&str>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let cursor = |code: char| {
        let prefix = if app_cursor { "\x1bO" } else { "\x1b[" };
        Some(format!("{prefix}{code}").into_bytes())
    };
    let fixed: Option<&[u8]> = match key {
        "enter" => Some(b"\r"),
        "backspace" => Some(if ctrl { b"\x08" } else { b"\x7f" }),
        "tab" if shift => Some(b"\x1b[Z"),
        "tab" => Some(b"\t"),
        "escape" => Some(b"\x1b"),
        "delete" => Some(b"\x1b[3~"),
        "insert" => Some(b"\x1b[2~"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        "f1" => Some(b"\x1bOP"),
        "f2" => Some(b"\x1bOQ"),
        "f3" => Some(b"\x1bOR"),
        "f4" => Some(b"\x1bOS"),
        "f5" => Some(b"\x1b[15~"),
        "f6" => Some(b"\x1b[17~"),
        "f7" => Some(b"\x1b[18~"),
        "f8" => Some(b"\x1b[19~"),
        "f9" => Some(b"\x1b[20~"),
        "f10" => Some(b"\x1b[21~"),
        "f11" => Some(b"\x1b[23~"),
        "f12" => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(fixed) = fixed {
        return Some(fixed.to_vec());
    }
    match key {
        "up" => return cursor('A'),
        "down" => return cursor('B'),
        "right" => return cursor('C'),
        "left" => return cursor('D'),
        "home" => return cursor('H'),
        "end" => return cursor('F'),
        _ => {}
    }
    // Ctrl+letter: its control code (Ctrl+C is ETX, the shell's interrupt).
    if ctrl && !alt {
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let code = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
                '[' => Some(0x1b),
                '\\' => Some(0x1c),
                ']' => Some(0x1d),
                ' ' | '2' | '@' => Some(0),
                _ => None,
            };
            return code.map(|b| vec![b]);
        }
        return None;
    }
    let text = typed?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    // Alt as Meta: ESC before the character.
    let mut out = if alt { vec![0x1b] } else { Vec::new() };
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(key: &str, typed: Option<&str>) -> Option<Vec<u8>> {
        bytes(key, typed, false, false, false, false)
    }

    #[test]
    fn keys_become_xterm_bytes() {
        assert_eq!(plain("a", Some("a")), Some(b"a".to_vec()));
        assert_eq!(plain("x", Some("ж")), Some("ж".as_bytes().to_vec()));
        assert_eq!(plain("enter", None), Some(b"\r".to_vec()));
        assert_eq!(plain("backspace", None), Some(vec![0x7f]));
        assert_eq!(plain("up", None), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            bytes("up", None, false, false, false, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(bytes("c", None, true, false, false, false), Some(vec![3]));
        assert_eq!(
            bytes("tab", None, false, false, true, false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            bytes("b", Some("b"), false, true, false, false),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(plain("shift", None), None);
    }
}
