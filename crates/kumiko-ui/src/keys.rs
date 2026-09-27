//! Keyboard input in the core's terms: a pressed key becomes a [`KeyChord`] that the keymap
//! resolves to a command (spec, section 22); bindings are shown as `Ctrl+Shift+P`.

use kumiko_core::config::keymap::{KeyChord, KeySequence, Keymap};
use kumiko_types::CommandId;

/// A key press as a chord; `None` for a lone modifier key.
pub fn chord(ctrl: bool, alt: bool, shift: bool, meta: bool, key: &str) -> Option<KeyChord> {
    let key = key.to_lowercase();
    if key.is_empty()
        || matches!(
            key.as_str(),
            "control" | "alt" | "shift" | "platform" | "function"
        )
    {
        return None;
    }
    Some(KeyChord {
        ctrl,
        shift,
        alt,
        meta,
        key,
    })
}

/// `Ctrl+Shift+P`; chords of a sequence are separated by a space.
pub fn label(sequence: &KeySequence) -> String {
    let chords: Vec<String> = sequence
        .0
        .iter()
        .map(|chord| {
            let mut parts = Vec::new();
            for (on, name) in [
                (chord.ctrl, "Ctrl"),
                (chord.alt, "Alt"),
                (chord.shift, "Shift"),
                (chord.meta, "Meta"),
            ] {
                if on {
                    parts.push(name.to_owned());
                }
            }
            parts.push(key_name(&chord.key));
            parts.join("+")
        })
        .collect();
    chords.join(" ")
}

fn key_name(key: &str) -> String {
    let function_key =
        key.len() > 1 && key.starts_with('f') && key[1..].chars().all(|c| c.is_ascii_digit());
    if key.chars().count() == 1 || function_key {
        return key.to_uppercase();
    }
    let mut chars = key.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The binding shown next to a command: the one a user layer would win with.
pub fn binding_label(keymap: &Keymap, command: &CommandId) -> Option<String> {
    keymap
        .bindings()
        .iter()
        .enumerate()
        .filter(|(_, b)| &b.command == command)
        .max_by_key(|(i, b)| (b.source, *i))
        .map(|(_, b)| label(&b.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_and_labels() {
        assert_eq!(chord(true, false, false, false, "shift"), None);
        let pressed = chord(true, false, true, false, "P").unwrap();
        assert_eq!(pressed, KeyChord::parse("ctrl+shift+p").unwrap());
        for (text, shown) in [
            ("ctrl+shift+p", "Ctrl+Shift+P"),
            ("alt+1", "Alt+1"),
            ("f5", "F5"),
            ("ctrl+k s", "Ctrl+K S"),
            ("shift+enter", "Shift+Enter"),
        ] {
            assert_eq!(label(&KeySequence::parse(text).unwrap()), shown, "{text}");
        }
    }
}
