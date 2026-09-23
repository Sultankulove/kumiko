//! Key bindings as a layer separate from commands (spec, section 22; P7).
//!
//! A binding maps a key sequence (`ctrl+s`, or a chord `ctrl+k ctrl+s`) to a command under
//! an optional when-expression. Precedence: user > kit > plugins > defaults; within one
//! source a more specific when-expression wins, then the binding added last.

use std::fmt;

use kumiko_service_api::when::{Context, WhenExpr};
use kumiko_types::CommandId;
use toml_edit::DocumentMut;

use crate::toml::to_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeymapSource {
    Default,
    Plugin,
    Kit,
    User,
}

/// One key press with modifiers, e.g. `ctrl+shift+o`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    /// Lowercase key name: `s`, `f5`, `enter`, `/`.
    pub key: String,
}

impl KeyChord {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut chord = KeyChord {
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
            key: String::new(),
        };
        let parts: Vec<&str> = text.split('+').collect();
        // `ctrl++` binds the plus key.
        let (modifiers, key) = match parts.as_slice() {
            [.., "", ""] => (&parts[..parts.len() - 2], "+"),
            [rest @ .., key] => (rest, *key),
            [] => return Err("empty key".into()),
        };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "option" => chord.alt = true,
                "meta" | "cmd" | "super" | "win" => chord.meta = true,
                other => return Err(format!("unknown modifier `{other}` in `{text}`")),
            }
        }
        if key.is_empty() {
            return Err(format!("missing key in `{text}`"));
        }
        chord.key = key.to_lowercase();
        Ok(chord)
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "ctrl+"),
            (self.shift, "shift+"),
            (self.alt, "alt+"),
            (self.meta, "meta+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key)
    }
}

/// A key sequence: one chord or several pressed one after another.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeySequence(pub Vec<KeyChord>);

impl KeySequence {
    pub fn parse(text: &str) -> Result<Self, String> {
        let chords = text
            .split_whitespace()
            .map(KeyChord::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if chords.is_empty() {
            return Err("empty key sequence".into());
        }
        Ok(Self(chords))
    }
}

impl fmt::Display for KeySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, chord) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{chord}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Keybinding {
    pub key: KeySequence,
    pub command: CommandId,
    pub when: Option<WhenExpr>,
    pub source: KeymapSource,
}

impl Keybinding {
    /// Number of `&&`-joined terms: a rough measure of how specific the condition is.
    fn specificity(&self) -> usize {
        self.when
            .as_ref()
            .map_or(0, |w| w.source().matches("&&").count() + 1)
    }
}

/// Result of looking up pressed keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Command(CommandId),
    /// The keys start a longer chord; wait for the next key.
    Pending,
    None,
}

/// Bindings that shadow each other: same keys, same condition, different commands.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub key: KeySequence,
    pub when: Option<String>,
    pub commands: Vec<(KeymapSource, CommandId)>,
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: Vec<Keybinding>,
}

/// Parses `[[binding]]` entries with `key`, `command` and optional `when`. Invalid entries
/// are skipped and reported.
pub fn parse_keymap(text: &str, source: KeymapSource) -> (Vec<Keybinding>, Vec<String>) {
    let mut problems = Vec::new();
    let document: DocumentMut = match text.parse() {
        Ok(document) => document,
        Err(error) => return (Vec::new(), vec![error.to_string()]),
    };
    let entries = match document.get("binding").map(to_json) {
        None => Vec::new(),
        Some(serde_json::Value::Array(entries)) => entries,
        Some(_) => {
            return (
                Vec::new(),
                vec!["`binding` must be an array of tables".into()],
            );
        }
    };
    let mut bindings = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let field = |name: &str| entry.get(name).and_then(serde_json::Value::as_str);
        let parsed = (|| {
            let key = KeySequence::parse(field("key").ok_or("missing `key`")?)?;
            let command = CommandId::new(field("command").ok_or("missing `command`")?)
                .map_err(|e| e.to_string())?;
            let when = field("when")
                .map(WhenExpr::parse)
                .transpose()
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(Keybinding {
                key,
                command,
                when,
                source,
            })
        })();
        match parsed {
            Ok(binding) => bindings.push(binding),
            Err(error) => problems.push(format!("binding {}: {error}", i + 1)),
        }
    }
    (bindings, problems)
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, binding: Keybinding) {
        self.bindings.push(binding);
    }

    /// Replaces all bindings of a source.
    pub fn set_source(&mut self, source: KeymapSource, bindings: Vec<Keybinding>) {
        self.bindings.retain(|b| b.source != source);
        self.bindings.extend(bindings);
    }

    pub fn bindings(&self) -> &[Keybinding] {
        &self.bindings
    }

    pub fn resolve(&self, pressed: &KeySequence, context: &dyn Context) -> Resolution {
        let active = |b: &&Keybinding| b.when.as_ref().is_none_or(|w| w.eval(context));
        let best = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.key == *pressed)
            .filter(|(_, b)| active(b))
            .max_by_key(|(i, b)| (b.source, b.specificity(), *i));
        if let Some((_, binding)) = best {
            return Resolution::Command(binding.command.clone());
        }
        let pending = self
            .bindings
            .iter()
            .filter(active)
            .any(|b| b.key.0.len() > pressed.0.len() && b.key.0.starts_with(&pressed.0));
        if pending {
            Resolution::Pending
        } else {
            Resolution::None
        }
    }

    /// Bindings of different commands to the same keys under the same condition.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut groups: Vec<Conflict> = Vec::new();
        for binding in &self.bindings {
            let when = binding.when.as_ref().map(|w| w.source().to_owned());
            match groups
                .iter_mut()
                .find(|g| g.key == binding.key && g.when == when)
            {
                Some(group) => group
                    .commands
                    .push((binding.source, binding.command.clone())),
                None => groups.push(Conflict {
                    key: binding.key.clone(),
                    when,
                    commands: vec![(binding.source, binding.command.clone())],
                }),
            }
        }
        groups.retain(|g| g.commands.iter().any(|(_, c)| c != &g.commands[0].1));
        groups
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::*;

    fn keys(text: &str) -> KeySequence {
        KeySequence::parse(text).unwrap()
    }

    fn binding(key: &str, command: &str, when: Option<&str>, source: KeymapSource) -> Keybinding {
        Keybinding {
            key: keys(key),
            command: CommandId::new(command).unwrap(),
            when: when.map(|w| WhenExpr::parse(w).unwrap()),
            source,
        }
    }

    fn context(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn command(name: &str) -> Resolution {
        Resolution::Command(CommandId::new(name).unwrap())
    }

    #[test]
    fn chords_parse_and_normalize() {
        assert_eq!(keys("Shift+Ctrl+O").to_string(), "ctrl+shift+o");
        assert_eq!(keys("ctrl+k  ctrl+s").0.len(), 2);
        assert_eq!(keys("ctrl++").0[0].key, "+");
        assert!(KeySequence::parse("hyper+x").is_err());
        assert!(KeySequence::parse("ctrl+").is_err());
        assert!(KeySequence::parse("  ").is_err());
    }

    #[test]
    fn precedence_and_specificity() {
        let mut keymap = Keymap::new();
        keymap.add(binding("ctrl+s", "file.save", None, KeymapSource::Default));
        keymap.add(binding(
            "ctrl+s",
            "sample.save",
            Some("editorLangId == sample"),
            KeymapSource::Plugin,
        ));
        keymap.add(binding("ctrl+s", "file.saveAll", None, KeymapSource::User));
        keymap.add(binding("f5", "run.start", None, KeymapSource::Default));
        keymap.add(binding(
            "f5",
            "run.debug",
            Some("debugAvailable"),
            KeymapSource::Default,
        ));
        keymap.add(binding(
            "f5",
            "run.debugFast",
            Some("debugAvailable && fast"),
            KeymapSource::Default,
        ));

        let empty = context(&[]);
        assert_eq!(
            keymap.resolve(&keys("ctrl+s"), &empty),
            command("file.saveAll")
        );
        keymap.set_source(KeymapSource::User, vec![]);
        assert_eq!(
            keymap.resolve(&keys("ctrl+s"), &empty),
            command("file.save")
        );
        let sample = context(&[("editorLangId", json!("sample"))]);
        assert_eq!(
            keymap.resolve(&keys("ctrl+s"), &sample),
            command("sample.save")
        );

        assert_eq!(keymap.resolve(&keys("f5"), &empty), command("run.start"));
        let debug = context(&[("debugAvailable", json!(true))]);
        assert_eq!(keymap.resolve(&keys("f5"), &debug), command("run.debug"));
        let fast = context(&[("debugAvailable", json!(true)), ("fast", json!(true))]);
        assert_eq!(keymap.resolve(&keys("f5"), &fast), command("run.debugFast"));
    }

    #[test]
    fn chords_wait_for_the_next_key() {
        let mut keymap = Keymap::new();
        keymap.add(binding(
            "ctrl+k ctrl+s",
            "keymap.open",
            None,
            KeymapSource::Default,
        ));
        let empty = context(&[]);
        assert_eq!(keymap.resolve(&keys("ctrl+k"), &empty), Resolution::Pending);
        assert_eq!(
            keymap.resolve(&keys("ctrl+k ctrl+s"), &empty),
            command("keymap.open")
        );
        assert_eq!(
            keymap.resolve(&keys("ctrl+k ctrl+x"), &empty),
            Resolution::None
        );
    }

    #[test]
    fn conflicts_need_same_keys_and_condition() {
        let mut keymap = Keymap::new();
        keymap.add(binding("ctrl+s", "file.save", None, KeymapSource::Default));
        keymap.add(binding("ctrl+s", "sample.save", None, KeymapSource::Plugin));
        keymap.add(binding(
            "ctrl+s",
            "other.save",
            Some("x"),
            KeymapSource::Plugin,
        ));
        keymap.add(binding("ctrl+p", "quick.open", None, KeymapSource::Default));
        keymap.add(binding("ctrl+p", "quick.open", None, KeymapSource::User));
        let conflicts = keymap.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, keys("ctrl+s"));
        assert_eq!(conflicts[0].commands.len(), 2);
    }

    #[test]
    fn keymap_files() {
        let text = r#"
            [[binding]]
            key = "ctrl+s"
            command = "file.save"
            when = "editorFocus"

            [[binding]]
            key = "ctrl+q"

            [[binding]]
            key = "ctrl+w"
            command = "view.close"
            when = "a &&"
        "#;
        let (bindings, problems) = parse_keymap(text, KeymapSource::User);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].command.as_str(), "file.save");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("binding 2"));
        let (_, problems) = parse_keymap("binding = 3", KeymapSource::User);
        assert_eq!(problems.len(), 1);
    }
}
