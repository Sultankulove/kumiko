//! The subset of JSON Schema used for settings: `type`, `enum`, `minimum`, `maximum`,
//! `items`, `default` and `description`. Other keywords are accepted and ignored.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("invalid settings schema: {0}")]
pub struct SchemaError(pub String);

/// Schema of one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertySchema {
    raw: Map<String, Value>,
}

impl PropertySchema {
    pub fn new(raw: Value) -> Result<Self, SchemaError> {
        match raw {
            Value::Object(raw) => Ok(Self { raw }),
            other => Err(SchemaError(format!(
                "property schema must be an object, got {other}"
            ))),
        }
    }

    pub fn default_value(&self) -> Option<&Value> {
        self.raw.get("default")
    }

    pub fn description(&self) -> Option<&str> {
        self.raw.get("description").and_then(Value::as_str)
    }

    /// A display name, when the schema gives one.
    pub fn title(&self) -> Option<&str> {
        self.raw.get("title").and_then(Value::as_str)
    }

    /// What kind of control edits the value.
    pub fn kind(&self) -> SettingKind {
        let bound = |name| self.raw.get(name).and_then(Value::as_f64);
        if let Some(Value::Array(values)) = self.raw.get("enum") {
            return SettingKind::Choice(values.clone());
        }
        let types = types(&self.raw);
        let has = |t: &str| types.iter().any(|x| x == t);
        if has("array") || has("object") {
            SettingKind::Structured
        } else if has("boolean") {
            SettingKind::Boolean
        } else if has("integer") || has("number") {
            SettingKind::Number {
                integer: !has("number"),
                minimum: bound("minimum"),
                maximum: bound("maximum"),
            }
        } else {
            // Strings, nullable strings and schemas without a type: edited as text.
            SettingKind::Text {
                nullable: has("null"),
            }
        }
    }

    /// Whether values of this setting are objects, i.e. TOML tables are not flattened.
    pub fn is_object(&self) -> bool {
        types(&self.raw).iter().any(|t| t == "object")
    }

    pub fn validate(&self, value: &Value) -> Result<(), String> {
        validate(&self.raw, value)
    }
}

/// The control a setting needs (Settings window, the `Form` model).
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Boolean,
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    /// Text; `nullable`: an empty field means `null`.
    Text {
        nullable: bool,
    },
    /// One of the listed values.
    Choice(Vec<Value>),
    /// Arrays and objects: edited in the settings file.
    Structured,
}

fn types(schema: &Map<String, Value>) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn validate(schema: &Map<String, Value>, value: &Value) -> Result<(), String> {
    let expected = types(schema);
    if !expected.is_empty() && !expected.iter().any(|t| type_matches(t, value)) {
        return Err(format!("expected {}, got {value}", expected.join(" or ")));
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.contains(value)
    {
        return Err(format!(
            "{value} is not one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let Some(number) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && number < min
        {
            return Err(format!("{number} is less than the minimum {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && number > max
        {
            return Err(format!("{number} is greater than the maximum {max}"));
        }
    }
    if let (Some(Value::Object(items)), Value::Array(values)) = (schema.get("items"), value) {
        for (i, item) in values.iter().enumerate() {
            validate(items, item).map_err(|e| format!("item {i}: {e}"))?;
        }
    }
    Ok(())
}

/// Parses `{"properties": {"<key>": {...}}}` into property schemas, checking defaults.
pub fn parse_properties(schema: &Value) -> Result<Vec<(String, PropertySchema)>, SchemaError> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Err(SchemaError("missing `properties` object".into()));
    };
    let mut parsed = Vec::with_capacity(properties.len());
    for (key, raw) in properties {
        let property = PropertySchema::new(raw.clone())?;
        if let Some(default) = property.default_value() {
            property
                .validate(default)
                .map_err(|e| SchemaError(format!("default of `{key}` is invalid: {e}")))?;
        }
        parsed.push((key.clone(), property));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn validation_subset() {
        let size =
            PropertySchema::new(json!({"type": "integer", "minimum": 6, "maximum": 100})).unwrap();
        assert!(size.validate(&json!(14)).is_ok());
        assert!(size.validate(&json!(14.5)).is_err());
        assert!(size.validate(&json!(2)).is_err());
        assert!(size.validate(&json!("14")).is_err());

        let eol = PropertySchema::new(json!({"enum": ["lf", "crlf", "auto"]})).unwrap();
        assert!(eol.validate(&json!("lf")).is_ok());
        assert!(eol.validate(&json!("cr")).is_err());

        let list =
            PropertySchema::new(json!({"type": ["array", "null"], "items": {"type": "string"}}))
                .unwrap();
        assert!(list.validate(&json!(["a"])).is_ok());
        assert!(list.validate(&Value::Null).is_ok());
        assert!(list.validate(&json!(["a", 1])).is_err());
    }

    #[test]
    fn defaults_are_checked() {
        let bad = json!({"properties": {"editor.tabSize": {"type": "integer", "default": "four"}}});
        assert!(parse_properties(&bad).is_err());
        assert!(parse_properties(&json!({})).is_err());
    }
}
