use broccoli_server_sdk::types::sanitize_text_field;
use serde_json::{Map, Value};

/// Read the opaque text exposed by plugin storage, including existing JSON values.
pub fn plugin_storage_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub fn sanitize_db_text(value: impl AsRef<str>) -> String {
    sanitize_text_field(value.as_ref()).into_owned()
}

pub fn sanitize_db_text_opt(value: Option<String>) -> Option<String> {
    value.map(sanitize_db_text)
}

pub fn sanitize_db_json(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(sanitize_db_text(s)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize_db_json).collect()),
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| (sanitize_db_text(key), sanitize_db_json(value)))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_db_text_replaces_nul() {
        assert_eq!(sanitize_db_text("a\0b"), "a\u{FFFD}b");
    }

    #[test]
    fn sanitize_db_json_replaces_nested_nul() {
        let value = serde_json::json!({
            "bad\0key": ["ok", "bad\0value", { "nested": "x\0y" }]
        });

        assert_eq!(
            sanitize_db_json(value),
            serde_json::json!({
                "bad\u{FFFD}key": ["ok", "bad\u{FFFD}value", { "nested": "x\u{FFFD}y" }]
            })
        );
    }
}
