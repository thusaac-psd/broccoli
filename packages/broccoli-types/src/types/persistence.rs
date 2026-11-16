use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::verdict::Verdict;

/// serde deserializer for the `Option<Option<T>>` tri-state fields that cross the
/// host boundary as JSON. Without it, serde folds an explicit `null` (meaning
/// "set this column to NULL") into the outer `None` (meaning "leave this column
/// untouched"), silently dropping a `SET ... = NULL`. Paired with
/// `skip_serializing_if = "Option::is_none"` on the same field this round-trips
/// faithfully: an absent field is the outer `None`, a present `null` is
/// `Some(None)`, and a present value is `Some(Some(v))`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// Serialize an `f64`, coercing non-finite values (NaN / +/-Inf) to `0.0`. JSON
/// has no representation for non-finite floats - serde_json emits `null`, which
/// then fails to read back as an `f64` - and the judge already coerces a
/// non-finite score to `0.0` before persisting it. Doing the coercion at the
/// wire boundary preserves that behavior while guaranteeing a valid number
/// crosses the boundary.
fn serialize_finite_f64<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(if v.is_finite() { *v } else { 0.0 })
}

/// `Option<f64>` counterpart of [`serialize_finite_f64`]. `None` stays absent
/// (the field is skipped); `Some(non-finite)` becomes `Some(0.0)` rather than a
/// wire `null`, which would otherwise be read back as "no score update".
fn serialize_finite_opt_f64<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => s.serialize_some(&(if x.is_finite() { *x } else { 0.0 })),
        None => s.serialize_none(),
    }
}

pub const RESULT_TEXT_DB_LIMIT_CHARS: usize = 64 * 1024;

pub fn sanitize_text_field(s: &str) -> Cow<'_, str> {
    if s.contains('\0') {
        Cow::Owned(s.replace('\0', "\u{FFFD}"))
    } else {
        Cow::Borrowed(s)
    }
}

pub fn sanitize_result_text_field(s: &str) -> Cow<'_, str> {
    let sanitized = sanitize_text_field(s);
    if sanitized.chars().count() <= RESULT_TEXT_DB_LIMIT_CHARS {
        return sanitized;
    }

    let mut truncated: String = sanitized.chars().take(RESULT_TEXT_DB_LIMIT_CHARS).collect();
    truncated.push_str("\n... (truncated)");
    Cow::Owned(truncated)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmissionStatus {
    Compiling,
    Running,
    Judged,
    CompilationError,
}

impl SubmissionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Compiling => "Compiling",
            Self::Running => "Running",
            Self::Judged => "Judged",
            Self::CompilationError => "CompilationError",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Judged | Self::CompilationError)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubmissionUpdate {
    pub submission_id: i32,
    /// Targeted `submission_judgement` row. The SDK writes the result
    /// fields to this judgement and mirrors the cache columns onto the
    /// owning submission row in the same statement. Must be positive.
    pub judgement_id: i32,
    pub judge_epoch: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubmissionStatus>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub verdict: Option<Option<Verdict>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_finite_opt_f64"
    )]
    pub score: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub time_used: Option<Option<i32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub memory_used: Option<Option<i32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub compile_output: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub error_code: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub error_message: Option<Option<String>>,
}

impl SubmissionUpdate {
    pub fn new(submission_id: i32, judgement_id: i32, judge_epoch: i32) -> Self {
        Self {
            submission_id,
            judgement_id,
            judge_epoch,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestCaseResultRow {
    pub submission_id: i32,
    /// Targeted `submission_judgement` row. The SDK writes this onto
    /// `test_case_result.judgement_id`. Must be positive.
    pub judgement_id: i32,
    pub judge_epoch: i32,
    pub test_case_id: Option<i32>,
    pub run_index: Option<i32>,
    pub verdict: Verdict,
    #[serde(serialize_with = "serialize_finite_f64")]
    pub score: f64,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodeRunUpdate {
    pub code_run_id: i32,
    pub judge_epoch: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubmissionStatus>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub verdict: Option<Option<Verdict>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_finite_opt_f64"
    )]
    pub score: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub time_used: Option<Option<i32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub memory_used: Option<Option<i32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub compile_output: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub error_code: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option"
    )]
    pub error_message: Option<Option<String>>,
}

impl CodeRunUpdate {
    pub fn new(code_run_id: i32) -> Self {
        Self {
            code_run_id,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeRunResultRow {
    pub code_run_id: i32,
    pub judge_epoch: i32,
    pub run_index: i32,
    pub verdict: Verdict,
    #[serde(serialize_with = "serialize_finite_f64")]
    pub score: f64,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_text_field_passes_through_clean_strings() {
        let s = "hello world\nline2";
        let out = sanitize_text_field(s);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "hello world\nline2");
    }

    #[test]
    fn sanitize_text_field_replaces_nul_with_replacement_char() {
        let s = "abc\0def\0\0xyz";
        let out = sanitize_text_field(s);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out, "abc\u{FFFD}def\u{FFFD}\u{FFFD}xyz");
        // 1 NUL byte -> 1 replacement char (not stripped), so char count is preserved.
        assert_eq!(out.chars().count(), s.chars().count());
    }

    #[test]
    fn sanitize_text_field_handles_empty_and_only_nul() {
        assert_eq!(sanitize_text_field(""), "");
        assert_eq!(sanitize_text_field("\0"), "\u{FFFD}");
        assert_eq!(sanitize_text_field("\0\0\0"), "\u{FFFD}\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn submission_update_json_roundtrip_preserves_tristate() {
        // The three states of a `Option<Option<T>>` field must survive a JSON
        // round-trip: absent (leave column), null (set NULL), value (set value).
        let leave = SubmissionUpdate {
            submission_id: 1,
            judgement_id: 2,
            judge_epoch: 3,
            ..Default::default()
        };
        let set_null = SubmissionUpdate {
            verdict: Some(None),
            time_used: Some(None),
            compile_output: Some(None),
            ..leave.clone()
        };
        let set_value = SubmissionUpdate {
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(42.5),
            time_used: Some(Some(10)),
            error_message: Some(Some("boom".to_string())),
            ..leave.clone()
        };

        for original in [leave, set_null, set_value] {
            let json = serde_json::to_string(&original).unwrap();
            let back: SubmissionUpdate = serde_json::from_str(&json).unwrap();
            assert_eq!(back, original, "round-trip changed value; json = {json}");
        }
    }

    #[test]
    fn submission_update_absent_and_null_are_distinct_on_the_wire() {
        // `None` (absent) and `Some(None)` (explicit null) must serialize
        // differently, otherwise the server cannot tell "leave it" from
        // "set NULL".
        let absent = SubmissionUpdate {
            submission_id: 1,
            judgement_id: 2,
            judge_epoch: 3,
            ..Default::default()
        };
        let explicit_null = SubmissionUpdate {
            verdict: Some(None),
            ..absent.clone()
        };
        let a = serde_json::to_string(&absent).unwrap();
        let b = serde_json::to_string(&explicit_null).unwrap();
        assert!(!a.contains("verdict"), "absent field must be omitted: {a}");
        assert!(b.contains("\"verdict\":null"), "null must be present: {b}");
    }

    #[test]
    fn code_run_update_round_trips_with_absent_and_null_distinct() {
        let update = CodeRunUpdate {
            code_run_id: 4,
            judge_epoch: 5,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(None),
            ..Default::default()
        };
        let wire = serde_json::to_string(&update).unwrap();
        assert!(
            !wire.contains("score"),
            "absent field must be omitted: {wire}"
        );
        assert!(
            wire.contains("\"verdict\":null"),
            "null must be present: {wire}"
        );
        let back: CodeRunUpdate = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn score_fields_coerce_non_finite_to_zero_on_the_wire() {
        let update = SubmissionUpdate {
            submission_id: 1,
            judgement_id: 2,
            judge_epoch: 3,
            score: Some(f64::NAN),
            ..Default::default()
        };
        let back: SubmissionUpdate =
            serde_json::from_str(&serde_json::to_string(&update).unwrap()).unwrap();
        assert_eq!(back.score, Some(0.0));

        let row = TestCaseResultRow {
            submission_id: 1,
            judgement_id: 2,
            judge_epoch: 3,
            test_case_id: Some(4),
            run_index: Some(0),
            verdict: Verdict::Accepted,
            score: f64::INFINITY,
            time_used: None,
            memory_used: None,
            message: None,
            stdout: None,
            stderr: None,
        };
        let back: TestCaseResultRow =
            serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(back.score, 0.0);
    }

    #[test]
    fn sanitize_result_text_field_truncates_large_values() {
        let input = "a".repeat(RESULT_TEXT_DB_LIMIT_CHARS + 10);
        let out = sanitize_result_text_field(&input);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(
            out.as_ref(),
            format!(
                "{}\n... (truncated)",
                "a".repeat(RESULT_TEXT_DB_LIMIT_CHARS)
            )
        );
    }
}
