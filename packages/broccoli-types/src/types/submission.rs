use serde::{Deserialize, Serialize};

use super::query::TestCaseRow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnSubmissionInput {
    pub submission_id: i32,
    /// ID of the active `submission_judgement` row this dispatch is for.
    /// Plugins must forward it onto every `SubmissionUpdate` and
    /// `TestCaseResultRow` they write so each result is attached to the
    /// right version. The server creates the v1 judgement at submit time
    /// and a new one on rejudge.
    #[serde(default)]
    pub judgement_id: i32,
    #[serde(default = "default_fire_after_judging")]
    pub fire_after_judging: bool,
    pub user_id: i32,
    pub problem_id: i32,
    pub contest_id: Option<i32>,
    pub files: Vec<SourceFile>,
    pub language: String,
    pub time_limit_ms: i32,
    pub memory_limit_kb: i32,
    pub problem_type: String,
    #[serde(default)]
    pub test_cases: Vec<TestCaseRow>,
    #[serde(default)]
    pub judge_epoch: i32,
    /// Pin every operation produced for this submission to a specific worker.
    /// Set by the server when an admin pinned the submission; contest plugins
    /// must forward it onto each `StartEvaluateCaseInput`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_worker_id: Option<String>,
}

fn default_fire_after_judging() -> bool {
    true
}

/// What a contest-type plugin's submission handler reports back. Build it
/// with [`OnSubmissionOutput::ok`], [`OnSubmissionOutput::failed`] or
/// [`OnSubmissionOutput::refused`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnSubmissionOutput {
    pub success: bool,
    pub error_message: Option<String>,
    /// `false` marks a failure as deterministic: the same submission would
    /// fail the same way every time (for example the contest type cannot
    /// judge a submission made outside a contest). The host records it as a
    /// terminal `SystemError` and never re-judges it automatically. Absent in
    /// older plugins' output, which reads as `true`.
    #[serde(default = "retryable_by_default", skip_serializing_if = "is_retryable")]
    pub retryable: bool,
}

fn retryable_by_default() -> bool {
    true
}

fn is_retryable(retryable: &bool) -> bool {
    *retryable
}

impl OnSubmissionOutput {
    /// The submission was handled.
    pub fn ok() -> Self {
        Self {
            success: true,
            error_message: None,
            retryable: true,
        }
    }

    /// Handling failed for a reason that may pass on a retry.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            success: false,
            error_message: Some(message.into()),
            retryable: true,
        }
    }

    /// Handling failed deterministically; see [`OnSubmissionOutput::retryable`].
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            success: false,
            error_message: Some(message.into()),
            retryable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OnSubmissionOutput;

    #[test]
    fn output_from_an_older_plugin_reads_as_retryable() {
        let out: OnSubmissionOutput =
            serde_json::from_str(r#"{"success":false,"error_message":"boom"}"#).unwrap();
        assert_eq!(out, OnSubmissionOutput::failed("boom"));
    }

    #[test]
    fn only_a_refusal_puts_retryable_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&OnSubmissionOutput::ok()).unwrap(),
            r#"{"success":true,"error_message":null}"#
        );
        let refused = serde_json::to_string(&OnSubmissionOutput::refused("no")).unwrap();
        assert_eq!(
            refused,
            r#"{"success":false,"error_message":"no","retryable":false}"#
        );
        let back: OnSubmissionOutput = serde_json::from_str(&refused).unwrap();
        assert!(!back.retryable);
    }
}
