use serde_json::Value as JsonValue;

use crate::types::sanitize_text_field;

pub struct Params {
    args: Vec<JsonValue>,
    idx: usize,
}

impl Params {
    pub fn new() -> Self {
        Self {
            args: Vec::new(),
            idx: 1,
        }
    }

    pub fn bind(&mut self, value: impl Into<JsonValue>) -> String {
        let placeholder = format!("${}", self.idx);
        self.args.push(sanitize_json_value(value.into()));
        self.idx += 1;
        placeholder
    }

    pub fn into_args(self) -> Vec<JsonValue> {
        self.args
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize_json_value(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::String(s) => JsonValue::String(sanitize_text_field(&s).into_owned()),
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(sanitize_json_value).collect())
        }
        JsonValue::Object(map) => JsonValue::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        sanitize_text_field(&key).into_owned(),
                        sanitize_json_value(value),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_replaces_nul_bytes_in_strings() {
        let mut params = Params::new();
        assert_eq!(params.bind("a\0b"), "$1");
        assert_eq!(
            params.into_args(),
            vec![JsonValue::String("a\u{FFFD}b".into())]
        );
    }

    #[test]
    fn bind_replaces_nul_bytes_in_nested_json() {
        let mut params = Params::new();
        params.bind(serde_json::json!({
            "std\0out": ["a\0b"]
        }));

        let args = params.into_args();
        assert_eq!(args[0]["std\u{FFFD}out"][0], "a\u{FFFD}b");
        assert!(!args[0].to_string().contains('\0'));
    }
}
