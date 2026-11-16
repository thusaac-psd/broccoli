//! Submission and judge result types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared::{ListPagination, Pagination};
use crate::{SubmissionStatus, Verdict};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionFileDto {
    #[schema(example = "solution.cpp")]
    pub filename: String,
    #[schema(example = "#include <iostream>\nint main() { return 0; }")]
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateSubmissionRequest {
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    #[schema(example = "ioi")]
    pub contest_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionResponse {
    #[schema(example = 1)]
    pub id: i32,
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    pub status: SubmissionStatus,
    #[schema(example = 1)]
    pub user_id: i32,
    #[schema(example = "alice")]
    pub username: String,
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "Two Sum")]
    pub problem_title: String,
    #[schema(example = 1)]
    pub contest_id: Option<i32>,
    #[schema(example = "ioi")]
    pub contest_type: String,
    #[schema(example = 0)]
    pub judge_epoch: i32,
    /// When set, the submission has been pinned to this worker by an admin
    /// and every operation it produces will run there.
    #[schema(example = "worker-1")]
    pub target_worker_id: Option<String>,
    #[schema(example = "2025-10-01T14:30:00Z")]
    pub created_at: DateTime<Utc>,
    pub result: Option<JudgeResultResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionListItem {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "cpp")]
    pub language: String,
    pub status: SubmissionStatus,
    #[schema(value_type = Option<String>, example = "Accepted")]
    pub verdict: Option<Verdict>,
    #[schema(example = 1)]
    pub user_id: i32,
    #[schema(example = "alice")]
    pub username: String,
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "Two Sum")]
    pub problem_title: String,
    pub contest_id: Option<i32>,
    #[schema(example = "ioi")]
    pub contest_type: String,
    #[schema(example = 0)]
    pub judge_epoch: i32,
    #[schema(example = "worker-1")]
    pub target_worker_id: Option<String>,
    #[schema(example = "2025-10-01T14:30:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(example = 100.0)]
    pub score: Option<f64>,
    #[schema(example = 50)]
    pub time_used: Option<i32>,
    #[schema(example = 1024)]
    pub memory_used: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionListResponse {
    pub data: Vec<SubmissionListItem>,
    pub pagination: ListPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JudgeResultResponse {
    #[serde(default)]
    pub judgement_id: Option<i32>,
    #[schema(value_type = Option<String>, example = "Accepted")]
    pub verdict: Option<Verdict>,
    #[schema(example = 100.0)]
    pub score: Option<f64>,
    #[schema(example = 50)]
    pub time_used: Option<i32>,
    #[schema(example = 1024)]
    pub memory_used: Option<i32>,
    pub compile_output: Option<String>,
    pub error_message: Option<String>,
    pub judged_at: Option<DateTime<Utc>>,
    pub test_case_results: Vec<TestCaseResultResponse>,
    #[serde(default)]
    pub test_case_pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestCaseResultResponse {
    #[schema(example = 1)]
    pub id: i32,
    /// `Option`, not `Verdict`: a `FieldMask` (e.g. `subtask_scores`'s
    /// `result.test_case_results.*.verdict`) can blank this to JSON `null`
    /// for a viewer who isn't entitled to the per-test-case breakdown, and a
    /// mask can only ever blank a value, never author a replacement - so the
    /// wire type has to admit `null` even though every row that reaches here
    /// unmasked always carries a real verdict. See
    /// `apply_filter_to_judgement_response` for where that null is produced.
    #[schema(value_type = Option<String>, example = "Accepted")]
    pub verdict: Option<Verdict>,
    /// `Option` for the same reason as `verdict` above (this field is masked
    /// by the same `result.test_case_results.*.score`-shaped `FieldMask`
    /// paths) - every row that reaches here unmasked always carries a real
    /// score.
    #[schema(example = 10.0)]
    pub score: Option<f64>,
    #[schema(example = 5)]
    pub time_used: Option<i32>,
    #[schema(example = 256)]
    pub memory_used: Option<i32>,
    pub test_case_id: Option<i32>,
    pub input: Option<String>,
    pub expected_output: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub checker_output: Option<String>,
}
