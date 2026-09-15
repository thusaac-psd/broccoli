use serde_json::Value;

use super::FieldMask;

/// Blank every path in `mask` inside `value`.
///
/// Blanking is destructive-only: a scalar becomes `null`, an array becomes
/// empty, and a path that does not exist is left alone. The function can never
/// insert or overwrite a value with plugin-supplied content — that is what makes
/// redaction safe to drive from a plugin response.
pub fn apply_mask(value: &mut Value, mask: &FieldMask) {
    for path in mask.paths() {
        blank_path(value, path);
    }
}

fn blank_path(value: &mut Value, path: &str) {
    let mut cursor = value;
    let mut parts = path.split('.').peekable();

    while let Some(part) = parts.next() {
        let Some(obj) = cursor.as_object_mut() else {
            return;
        };
        let Some(next) = obj.get_mut(part) else {
            return;
        };
        if parts.peek().is_none() {
            *next = match next {
                Value::Array(_) => Value::Array(Vec::new()),
                _ => Value::Null,
            };
            return;
        }
        cursor = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blanks_a_nested_scalar_to_null() {
        let mut v = json!({"result": {"verdict": "AC", "score": 100}});
        apply_mask(&mut v, &FieldMask::new(["result.verdict".to_string()]));
        assert_eq!(v, json!({"result": {"verdict": null, "score": 100}}));
    }

    #[test]
    fn blanks_an_array_to_empty_not_null() {
        let mut v = json!({"result": {"test_case_results": [{"verdict": "AC"}]}});
        apply_mask(&mut v, &FieldMask::new(["result.test_case_results".to_string()]));
        assert_eq!(v, json!({"result": {"test_case_results": []}}));
    }

    #[test]
    fn blanks_a_top_level_field() {
        let mut v = json!({"verdict": "AC", "id": 1});
        apply_mask(&mut v, &FieldMask::new(["verdict".to_string()]));
        assert_eq!(v, json!({"verdict": null, "id": 1}));
    }

    #[test]
    fn missing_path_is_a_no_op_not_an_insert() {
        let mut v = json!({"id": 1});
        apply_mask(&mut v, &FieldMask::new(["result.verdict".to_string()]));
        assert_eq!(v, json!({"id": 1}), "mask must never create a field");
    }

    #[test]
    fn mask_never_writes_a_value() {
        let mut v = json!({"result": {"verdict": "WA"}});
        apply_mask(&mut v, &FieldMask::new(["result.verdict".to_string()]));
        assert!(v["result"]["verdict"].is_null());
    }
}
