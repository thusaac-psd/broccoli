//! Shared submission-detail visibility helpers for contest plugins.
//!
//! The per-contest-type plugins (icpc, ioi, ...) each echo a submission's
//! free-text fields back in their submission-detail responses. Capping those
//! fields is scoreboard/payload-integrity logic that MUST be identical across
//! plugins, so it lives here rather than being copy-pasted into each. (The
//! plugin-specific parts - icpc's freeze-hiding, ioi's feedback-level redaction
//! - stay in their own crates.)

/// The submission field whose hiding withholds the submission's whole
/// outcome. A visibility decision that redacts this path presents the
/// submission to that viewer as still running with no result (no verdict,
/// score, resources or test case results), and a submission list filtered or
/// sorted by status does not place it, so neither reveals the outcome.
pub const SUBMISSION_STATUS_PATH: &str = "status";

/// Byte cap for a free-text field echoed back in a submission-detail response,
/// so a pathological compiler dump / checker output / test-case blob cannot
/// bloat the API payload. Shared so the bound cannot drift between plugins.
pub const DETAIL_TEXT_RESPONSE_LIMIT_BYTES: usize = 65_536;

/// Truncate a string to at most [`DETAIL_TEXT_RESPONSE_LIMIT_BYTES`] bytes,
/// never splitting a multi-byte UTF-8 character.
fn cap_detail_text(mut value: String) -> String {
    if value.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES {
        return value;
    }

    let mut end = DETAIL_TEXT_RESPONSE_LIMIT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn cap_json_string_field(map: &mut serde_json::Map<String, serde_json::Value>, field: &str) {
    let Some(value) = map.get_mut(field) else {
        return;
    };
    let Some(text) = value.as_str() else {
        return;
    };
    if text.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES {
        return;
    }
    *value = serde_json::Value::String(cap_detail_text(text.to_string()));
}

/// Cap the free-text fields of a submission-detail JSON value in place:
/// `result.compile_output` / `result.error_message`, and each test case's
/// `input` / `expected_output` / `stdout` / `stderr` / `checker_output`.
pub fn cap_submission_detail_texts(submission: &mut serde_json::Value) {
    let Some(result) = submission.get_mut("result").and_then(|v| v.as_object_mut()) else {
        return;
    };

    for field in ["compile_output", "error_message"] {
        cap_json_string_field(result, field);
    }

    let Some(test_case_results) = result
        .get_mut("test_case_results")
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };

    for test_case_result in test_case_results {
        let Some(test_case_result) = test_case_result.as_object_mut() else {
            continue;
        };
        for field in [
            "input",
            "expected_output",
            "stdout",
            "stderr",
            "checker_output",
        ] {
            cap_json_string_field(test_case_result, field);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_oversized_detail_texts() {
        let big = "x".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES + 10);
        let mut sub = serde_json::json!({
            "result": {
                "compile_output": big,
                "test_case_results": [{"checker_output": "y".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES * 2)}]
            }
        });
        cap_submission_detail_texts(&mut sub);
        let capped = sub["result"]["compile_output"].as_str().unwrap();
        assert_eq!(capped.len(), DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
        let tc = sub["result"]["test_case_results"][0]["checker_output"]
            .as_str()
            .unwrap();
        assert_eq!(tc.len(), DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
    }

    #[test]
    fn cap_respects_char_boundaries() {
        // Multi-byte char straddling the limit must not split.
        let mut s = "a".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES - 1);
        s.push('é'); // 2 bytes, crosses the boundary
        s.push_str("tail");
        let capped = cap_detail_text(s);
        assert!(capped.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
        assert!(capped.is_char_boundary(capped.len()));
    }
}
