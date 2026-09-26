use serde_json::Value;

use super::FieldMask;

/// Blank every path in `mask` inside `value`.
///
/// A path is a `.`-separated sequence of segments. Any segment other than
/// `*` is looked up by key in the current object. A `*` segment instead
/// requires the current node to be an array and fans out into EVERY element,
/// applying the remaining segments to each independently (so a path may
/// contain more than one `*`, addressing arrays nested inside arrays).
///
/// Blanking is destructive-only: a scalar becomes `null`, an array becomes
/// empty, and a path that does not exist — including a `*` segment landing on
/// a non-array, an empty array, or an element missing the remaining
/// path — is left alone. The function can never insert or overwrite a value
/// with plugin-supplied content — that is what makes redaction safe to drive
/// from a plugin response. Callers on the untrusted-input path (plugin
/// responses) MUST bound path length/segment count/field count before
/// calling this function — see `MAX_MASK_PATH_SEGMENTS` et al. in
/// `plugin_query.rs` — since this function recurses once per matched `*`
/// segment and imposes no depth limit of its own.
///
/// A path that simply doesn't match anything in `value` is deliberately left
/// undiagnosed: ICPC and IOI each ship one *union* `FieldMask` reused across
/// several different resource shapes (submission detail, standings row,
/// etc.), so "most of these paths don't exist in this particular shape" is
/// the normal, intended case, not a mistake — a real typo in a path segment
/// is indistinguishable from that at this layer and cannot be flagged
/// without drowning the genuine case in noise. What CAN be flagged
/// unambiguously is a path that is syntactically malformed — an empty
/// segment from a leading dot, a trailing dot, or a doubled dot (`.result`,
/// `result.`, `result..verdict`) — since a struct field name is never the
/// empty string, so a path like that can never match anything, ever,
/// regardless of shape. `has_empty_segment` (private, below) catches exactly
/// that narrower case and logs it at debug level; it does not change
/// blanking behavior (the path still silently no-ops, same as before) or
/// reject the mask.
pub fn apply_mask(value: &mut Value, mask: &FieldMask) {
    for path in mask.paths() {
        if has_empty_segment(path) {
            tracing::debug!(
                path,
                "mask path has an empty segment (leading/trailing/doubled '.'); \
                 it is syntactically incapable of matching a field and will \
                 silently blank nothing"
            );
        }
        blank_path(value, path);
    }
}

/// True if `path` is empty, or splitting it on `.` yields an empty segment
/// (leading dot, trailing dot, or a doubled dot). See [`apply_mask`]'s doc
/// comment for why this — and only this — is worth diagnosing.
fn has_empty_segment(path: &str) -> bool {
    path.is_empty() || path.split('.').any(str::is_empty)
}

fn blank_path(value: &mut Value, path: &str) {
    let segments: Vec<&str> = path.split('.').collect();
    blank_segments(value, &segments);
}

/// Recursively walk `segments` against `cursor`.
///
/// `*` requires the current node to be an array and recurses into every
/// element with the remaining segments; a non-array is a no-op. Any other
/// segment requires an object and looks the segment up by key; a miss is a
/// no-op. At the last segment the target is blanked: an array becomes empty,
/// everything else becomes `null`.
fn blank_segments(cursor: &mut Value, segments: &[&str]) {
    let Some((segment, rest)) = segments.split_first() else {
        return;
    };

    if *segment == "*" {
        let Value::Array(elements) = cursor else {
            return;
        };
        for element in elements.iter_mut() {
            if rest.is_empty() {
                blank_in_place(element);
            } else {
                blank_segments(element, rest);
            }
        }
        return;
    }

    let Some(obj) = cursor.as_object_mut() else {
        return;
    };
    let Some(next) = obj.get_mut(*segment) else {
        return;
    };

    if rest.is_empty() {
        blank_in_place(next);
        return;
    }

    blank_segments(next, rest);
}

/// Blank a single target in place: an array becomes empty, everything else
/// becomes `null`.
fn blank_in_place(target: &mut Value) {
    *target = match target {
        Value::Array(_) => Value::Array(Vec::new()),
        _ => Value::Null,
    };
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
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results".to_string()]),
        );
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

    #[test]
    fn wildcard_blanks_a_field_in_every_array_element() {
        let mut v = json!({"result": {"test_case_results": [
            {"verdict": "AC", "score": 10},
            {"verdict": "WA", "score": 0}
        ]}});
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results.*.verdict".to_string()]),
        );
        assert_eq!(
            v,
            json!({"result": {"test_case_results": [
                {"verdict": null, "score": 10},
                {"verdict": null, "score": 0}
            ]}})
        );
    }

    #[test]
    fn wildcard_on_an_empty_array_is_a_no_op() {
        let mut v = json!({"result": {"test_case_results": []}});
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results.*.verdict".to_string()]),
        );
        assert_eq!(v, json!({"result": {"test_case_results": []}}));
    }

    #[test]
    fn wildcard_on_a_non_array_is_a_no_op() {
        let mut v = json!({"result": {"test_case_results": {"not": "an array"}}});
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results.*.verdict".to_string()]),
        );
        assert_eq!(
            v,
            json!({"result": {"test_case_results": {"not": "an array"}}})
        );
    }

    #[test]
    fn wildcard_never_creates_a_field_in_an_element() {
        let mut v = json!({"result": {"test_case_results": [{"score": 10}]}});
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results.*.verdict".to_string()]),
        );
        assert_eq!(
            v,
            json!({"result": {"test_case_results": [{"score": 10}]}}),
            "wildcard must not insert a missing key into an element"
        );
    }

    #[test]
    fn trailing_wildcard_empties_each_element_that_is_an_array() {
        let mut v = json!({"groups": [{"cases": [1, 2]}, {"cases": [3]}]});
        apply_mask(&mut v, &FieldMask::new(["groups.*.cases".to_string()]));
        assert_eq!(v, json!({"groups": [{"cases": []}, {"cases": []}]}));
    }

    #[test]
    fn nested_wildcards_are_supported() {
        let mut v = json!({"a": [{"b": [{"c": 1}]}]});
        apply_mask(&mut v, &FieldMask::new(["a.*.b.*.c".to_string()]));
        assert_eq!(v, json!({"a": [{"b": [{"c": null}]}]}));
    }

    #[test]
    fn wildcard_blanks_only_elements_that_have_the_field() {
        let mut v = json!({"result": {"test_case_results": [
            {"verdict": "AC", "score": 10},
            {"score": 0}
        ]}});
        apply_mask(
            &mut v,
            &FieldMask::new(["result.test_case_results.*.verdict".to_string()]),
        );
        assert_eq!(
            v,
            json!({"result": {"test_case_results": [
                {"verdict": null, "score": 10},
                {"score": 0}
            ]}}),
            "elements missing the field must stay untouched, not gain a null key"
        );
    }

    #[test]
    fn wildcard_and_plain_path_apply_together() {
        let mut v = json!({"result": {
            "test_case_results": [
                {"verdict": "AC", "score": 10},
                {"verdict": "WA", "score": 0}
            ],
            "total_score": 10
        }});
        apply_mask(
            &mut v,
            &FieldMask::new([
                "result.test_case_results.*.verdict".to_string(),
                "result.total_score".to_string(),
            ]),
        );
        assert_eq!(
            v,
            json!({"result": {
                "test_case_results": [
                    {"verdict": null, "score": 10},
                    {"verdict": null, "score": 0}
                ],
                "total_score": null
            }})
        );
    }

    #[test]
    fn bare_wildcard_blanks_each_scalar_element() {
        let mut v = json!({"scores": [1, 2, 3]});
        apply_mask(&mut v, &FieldMask::new(["scores.*".to_string()]));
        assert_eq!(v, json!({"scores": [null, null, null]}));
    }

    // M3: a syntactically malformed path (empty/leading/trailing/doubled dot)
    // must still be a safe no-op — `has_empty_segment` only adds a debug-level
    // diagnostic, it never changes blanking behavior or rejects the mask.

    #[test]
    fn trailing_dot_path_has_empty_segment() {
        assert!(has_empty_segment("result."));
    }

    #[test]
    fn leading_dot_path_has_empty_segment() {
        assert!(has_empty_segment(".result"));
    }

    #[test]
    fn doubled_dot_path_has_empty_segment() {
        assert!(has_empty_segment("result..verdict"));
    }

    #[test]
    fn empty_path_has_empty_segment() {
        assert!(has_empty_segment(""));
    }

    #[test]
    fn well_formed_path_has_no_empty_segment() {
        assert!(!has_empty_segment("result.verdict"));
        assert!(!has_empty_segment("result.test_case_results.*.verdict"));
        assert!(!has_empty_segment("verdict"));
    }

    #[test]
    fn trailing_dot_path_still_blanks_nothing() {
        let mut v = json!({"result": {"verdict": "AC"}});
        apply_mask(&mut v, &FieldMask::new(["result.".to_string()]));
        assert_eq!(
            v,
            json!({"result": {"verdict": "AC"}}),
            "malformed path must stay a no-op, not start matching or panicking"
        );
    }

    #[test]
    fn leading_dot_path_still_blanks_nothing() {
        let mut v = json!({"result": {"verdict": "AC"}});
        apply_mask(&mut v, &FieldMask::new([".result".to_string()]));
        assert_eq!(v, json!({"result": {"verdict": "AC"}}));
    }

    #[test]
    fn doubled_dot_path_still_blanks_nothing() {
        let mut v = json!({"result": {"verdict": "AC"}});
        apply_mask(&mut v, &FieldMask::new(["result..verdict".to_string()]));
        assert_eq!(v, json!({"result": {"verdict": "AC"}}));
    }
}
