//! Validated string identifiers and numeric document ids.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// A string that does not satisfy the syntax of an identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} `{value}`: {reason}")]
pub struct IdError {
    pub kind: &'static str,
    pub value: String,
    pub reason: &'static str,
}

/// Defines a validated string identifier with serde and JSON Schema support.
macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident, $kind:literal, $pattern:literal, $validate:path) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                match $validate(&value) {
                    Ok(()) => Ok(Self(value)),
                    Err(reason) => Err(IdError { kind: $kind, value, reason }),
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, IdError> {
                Self::new(value)
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, IdError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern })
            }
        }
    };
}

string_id!(
    /// Reverse-DNS plugin id, e.g. `kumiko.git`: two or more dot-separated lowercase segments.
    PluginId,
    "plugin id",
    "^[a-z][a-z0-9-]*(\\.[a-z][a-z0-9-]*)+$",
    validate_reverse_dns
);

string_id!(
    /// Versioned inter-plugin interface id, e.g. `kumiko.git.repository` (same syntax as plugin ids).
    InterfaceId,
    "interface id",
    "^[a-z][a-z0-9-]*(\\.[a-z][a-z0-9-]*)+$",
    validate_reverse_dns
);

string_id!(
    /// Command id `namespace.action`, e.g. `file.save` or `workbench.action.quickOpen`.
    CommandId,
    "command id",
    "^[A-Za-z][A-Za-z0-9_-]*(\\.[A-Za-z][A-Za-z0-9_-]*)+$",
    validate_command
);

fn validate_segments(value: &str, allowed: impl Fn(char) -> bool) -> Result<(), &'static str> {
    let mut count = 0;
    for segment in value.split('.') {
        let mut chars = segment.chars();
        match chars.next() {
            None => return Err("empty segment"),
            Some(first) if !first.is_ascii_alphabetic() => {
                return Err("segment must start with a letter");
            }
            Some(_) => {}
        }
        if !chars.all(&allowed) {
            return Err("unexpected character");
        }
        count += 1;
    }
    if count < 2 {
        return Err("at least two dot-separated segments are required");
    }
    Ok(())
}

fn validate_reverse_dns(value: &str) -> Result<(), &'static str> {
    if value.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("must be lowercase");
    }
    validate_segments(value, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
    })
}

fn validate_command(value: &str) -> Result<(), &'static str> {
    validate_segments(value, |c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Identity of an open document inside the core; stable while the document stays open.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct DocumentId(pub u64);

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "doc#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids() {
        for ok in ["kumiko.git", "org.example.my-plugin", "a.b2"] {
            assert!(PluginId::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "kumiko",
            "Kumiko.git",
            "kumiko..git",
            ".kumiko",
            "kumiko.1git",
            "kumiko.g_t",
        ] {
            assert!(PluginId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn command_ids() {
        for ok in ["file.save", "workbench.action.quickOpen", "java.select_jdk"] {
            assert!(CommandId::new(ok).is_ok(), "{ok}");
        }
        for bad in ["save", "file.", "file.save!", "file .save"] {
            assert!(CommandId::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn serde_validates_ids() {
        let id: PluginId = serde_json::from_str("\"kumiko.git\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"kumiko.git\"");
        assert!(serde_json::from_str::<PluginId>("\"git\"").is_err());
        assert_eq!(serde_json::to_string(&DocumentId(7)).unwrap(), "7");
    }
}
