//! TOML settings files ↔ JSON values.

use serde_json::{Map, Number, Value};
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// Converts a TOML item into JSON. Datetimes become strings.
pub fn to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(value) => value_to_json(value),
        Item::Table(table) => table_to_json(table),
        Item::ArrayOfTables(tables) => {
            Value::Array(tables.iter().map(|table| table_to_json(table)).collect())
        }
    }
}

fn table_to_json(table: &dyn TableLike) -> Value {
    Value::Object(
        table
            .iter()
            .map(|(key, item)| (key.to_owned(), to_json(item)))
            .collect(),
    )
}

fn value_to_json(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => {
            Number::from_f64(*f.value()).map_or(Value::Null, Value::Number)
        }
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(array) => Value::Array(array.iter().map(value_to_json).collect()),
        toml_edit::Value::InlineTable(table) => table_to_json(table),
    }
}

/// Converts JSON into a TOML value; `None` for `null`, which TOML cannot express.
pub fn from_json(value: &Value) -> Option<toml_edit::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64()?.into(),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(from_json(item)?);
            }
            array.into()
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (key, item) in map {
                if let Some(item) = from_json(item) {
                    table.insert(key, item);
                }
            }
            table.into()
        }
    })
}

/// Flattens nested tables into dotted keys: `[editor] fontSize = 14` and
/// `"editor.fontSize" = 14` both become `editor.fontSize`. A key for which `keep_object`
/// is true keeps its table as an object value.
pub fn flatten(
    object: &Map<String, Value>,
    prefix: &str,
    keep_object: &dyn Fn(&str) -> bool,
    out: &mut Map<String, Value>,
) {
    for (key, value) in object {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(inner) if !keep_object(&full) => flatten(inner, &full, keep_object, out),
            _ => {
                out.insert(full, value.clone());
            }
        }
    }
}

/// Sets (or with `None` removes) a setting in a document, removing other spellings of the
/// same key first so the file keeps one definition. Comments elsewhere are preserved.
pub fn set(document: &mut DocumentMut, key: &str, value: Option<toml_edit::Value>) {
    remove_nested(document.as_table_mut(), key);
    if let Some(value) = value {
        document.as_table_mut().insert(key, Item::Value(value));
    }
}

/// Removes `key` however it is spelled: quoted (`"a.b.c"`), nested (`[a] b.c`, `[a.b] c`)
/// or mixed (`["a.b"] c`), by trying every split point of the dotted key.
fn remove_nested(table: &mut Table, key: &str) {
    table.remove(key);
    for (split, _) in key.match_indices('.') {
        let (head, rest) = (&key[..split], &key[split + 1..]);
        if let Some(inner) = table.get_mut(head).and_then(Item::as_table_mut) {
            remove_nested(inner, rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn nested_and_quoted_keys_flatten_alike() {
        let doc: DocumentMut =
            "\"editor.fontSize\" = 14\n[files]\neol = \"lf\"\nexclude = { a = true }\n"
                .parse()
                .unwrap();
        let Value::Object(tree) = to_json(doc.as_item()) else {
            panic!("expected a table");
        };
        let mut flat = Map::new();
        flatten(&tree, "", &|key| key == "files.exclude", &mut flat);
        assert_eq!(
            Value::Object(flat),
            json!({"editor.fontSize": 14, "files.eol": "lf", "files.exclude": {"a": true}})
        );
    }

    #[test]
    fn set_replaces_every_spelling_and_keeps_comments() {
        let mut doc: DocumentMut = "# my settings\n[editor]\nfontSize = 12 # small\ntabSize = 4\n"
            .parse()
            .unwrap();
        set(&mut doc, "editor.fontSize", from_json(&json!(16)));
        let text = doc.to_string();
        assert!(text.contains("# my settings"));
        assert!(text.contains("\"editor.fontSize\" = 16"), "{text}");
        assert!(!text.contains("fontSize = 12"), "{text}");
        assert!(text.contains("tabSize = 4"));

        set(&mut doc, "editor.fontSize", None);
        assert!(!doc.to_string().contains("fontSize"));
    }

    #[test]
    fn json_values_convert_back_and_forth() {
        let value = json!({"a": [1, 2.5, "x", true], "b": {"c": "d"}});
        let toml = from_json(&value).unwrap();
        assert_eq!(value_to_json(&toml), value);
        assert!(from_json(&Value::Null).is_none());
    }
}
