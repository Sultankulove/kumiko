//! Published artifacts derived from the catalog: the JSON Schema of the protocol
//! (`sdk/schema/kpp.schema.json`) and the method tables of `docs/protocol/v1.md`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::catalog::{MethodKind, catalog, core_events};
use crate::events::DeliveryClass;
use crate::jsonrpc::{ErrorCode, RequestId, ResponseError};
use crate::{Direction, PROTOCOL_VERSION};

/// JSON Schema (draft 2020-12) of every method payload and core event, with shared `$defs`.
pub fn protocol_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    let mut methods = Map::new();
    for method in catalog() {
        let mut entry = Map::new();
        entry.insert("direction".into(), json!(method.direction));
        entry.insert("kind".into(), json!(kind_name(method.kind)));
        entry.insert("description".into(), json!(method.doc));
        entry.insert(
            "params".into(),
            (method.params_schema)(&mut generator).into(),
        );
        if let Some(result_schema) = method.result_schema {
            entry.insert("result".into(), result_schema(&mut generator).into());
        }
        methods.insert(method.name.into(), Value::Object(entry));
    }

    let mut events = Map::new();
    for event in core_events() {
        events.insert(
            event.topic.into(),
            json!({
                "class": event.class,
                "description": event.doc,
                "payload": Value::from((event.payload_schema)(&mut generator)),
            }),
        );
    }

    let error_codes: Map<String, Value> = ErrorCode::ALL
        .iter()
        .map(|code| (code.name().to_owned(), json!(code.code())))
        .collect();

    let envelope = json!({
        "requestId": Value::from(generator.subschema_for::<RequestId>()),
        "error": Value::from(generator.subschema_for::<ResponseError>()),
    });

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Kumiko Plugin Protocol",
        "version": PROTOCOL_VERSION,
        "envelope": envelope,
        "errorCodes": error_codes,
        "methods": methods,
        "events": events,
        "$defs": generator.take_definitions(true),
    })
}

/// Pretty-printed schema exactly as committed to `sdk/schema/kpp.schema.json`.
pub fn protocol_schema_file() -> String {
    let mut text = serde_json::to_string_pretty(&protocol_schema()).unwrap_or_default();
    text.push('\n');
    text
}

fn kind_name(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::Request => "request",
        MethodKind::Notification => "notification",
    }
}

fn direction_arrow(direction: Direction) -> &'static str {
    match direction {
        Direction::HostToPlugin => "H→P",
        Direction::PluginToHost => "P→H",
        Direction::Both => "both",
    }
}

/// Markdown tables of methods and events for the generated block of `docs/protocol/v1.md`.
pub fn catalog_markdown() -> String {
    let mut out = String::from("| Method | Direction | Kind | Description |\n|---|---|---|---|\n");
    for method in catalog() {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            method.name,
            direction_arrow(method.direction),
            kind_name(method.kind),
            method.doc
        ));
    }
    out.push_str("\n| Event | Class | Description |\n|---|---|---|\n");
    for event in core_events() {
        let class = match event.class {
            DeliveryClass::State => "A (state)",
            DeliveryClass::Notification => "B (notification)",
        };
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            event.topic, class, event.doc
        ));
    }
    out.push_str("\n| Error | Code |\n|---|---|\n");
    for code in ErrorCode::ALL {
        out.push_str(&format!("| `{}` | {} |\n", code.name(), code.code()));
    }
    out
}
