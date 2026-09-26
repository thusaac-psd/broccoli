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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnSubmissionOutput {
    pub success: bool,
    pub error_message: Option<String>,
}
