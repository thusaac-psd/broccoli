use chrono::{DateTime, Utc};
use common::{SubmissionStatus, Verdict};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

use super::shared::Pagination;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionFile {
    pub filename: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SubmissionFileDto {
    #[schema(example = "solution.cpp")]
    pub filename: String,
    #[schema(example = "#include <iostream>\nint main() { return 0; }")]
    pub content: String,
}

impl From<SubmissionFileDto> for SubmissionFile {
    fn from(dto: SubmissionFileDto) -> Self {
        Self {
            filename: dto.filename,
            content: dto.content,
        }
    }
}

impl From<SubmissionFile> for SubmissionFileDto {
    fn from(file: SubmissionFile) -> Self {
        Self {
            filename: file.filename,
            content: file.content,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateSubmissionRequest {
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    #[schema(example = "ioi")]
    pub contest_type: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SubmissionListQuery {
    #[param(example = 1)]
    pub page: Option<u64>,
    #[param(example = 20)]
    pub per_page: Option<u64>,
    #[param(example = 1)]
    pub problem_id: Option<i32>,
    #[param(example = 1)]
    pub user_id: Option<i32>,
    #[param(example = "cpp")]
    pub language: Option<String>,
    pub status: Option<SubmissionStatus>,
    /// Free-text search across username, problem title, and contest title (case-insensitive).
    #[param(example = "alice")]
    pub q: Option<String>,
    #[param(example = "created_at")]
    pub sort_by: Option<String>,
    #[param(example = "desc")]
    pub sort_order: Option<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct SubmissionListResponse {
    pub data: Vec<SubmissionListItem>,
    pub pagination: Pagination,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JudgeResultResponse {
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
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionJudgementResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = 1)]
    pub submission_id: i32,
    #[schema(example = 2)]
    pub version: i32,
    pub is_current: bool,
    pub is_finalized: bool,
    pub status: SubmissionStatus,
    #[schema(value_type = Option<String>, example = "Accepted")]
    pub verdict: Option<Verdict>,
    #[schema(example = 100.0)]
    pub score: Option<f64>,
    #[schema(example = 50)]
    pub time_used: Option<i32>,
    #[schema(example = 1024)]
    pub memory_used: Option<i32>,
    pub compile_output: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    #[schema(example = 1)]
    pub judge_epoch: i32,
    #[schema(example = "worker-1")]
    pub target_worker_id: Option<String>,
    #[schema(example = "2025-10-01T14:30:00Z")]
    pub created_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub test_case_results: Vec<TestCaseResultResponse>,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkRejudgeRequest {
    pub submission_ids: Vec<i32>,
    /// When true (default), the new judgement becomes current immediately
    /// and the submission cache is reset to Pending. When false, the new
    /// judgement runs as a non-current candidate until explicitly applied.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub apply_immediately: bool,
    /// When set, every rejudged submission is pinned to this worker. The
    /// caller must hold `system:admin` and the worker must have a live
    /// heartbeat. An empty string clears existing pins and also requires
    /// `system:admin`.
    #[serde(default)]
    pub target_worker_id: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RejudgeRequest {
    /// When true (default), the new judgement becomes current immediately
    /// and the submission cache is reset to Pending. When false, the new
    /// judgement runs as a non-current candidate until explicitly applied.
    #[serde(default = "default_true")]
    #[schema(default = true)]
    pub apply_immediately: bool,
    /// Pin the rejudged submission to a specific worker. Requires
    /// `system:admin`. An empty string clears an existing pin and also
    /// requires `system:admin`. When omitted, the submission is rejudged on the
    /// shared worker pool.
    #[serde(default)]
    pub target_worker_id: Option<String>,
}

impl Default for RejudgeRequest {
    fn default() -> Self {
        Self {
            apply_immediately: true,
            target_worker_id: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AdminFanOutSubmissionRequest {
    #[schema(example = 1)]
    pub problem_id: i32,
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    #[schema(example = 1)]
    #[serde(default)]
    pub contest_id: Option<i32>,
    #[schema(example = "ioi")]
    #[serde(default)]
    pub contest_type: Option<String>,
    /// Workers to fan out to. One submission is created per worker. Each ID
    /// must have a live heartbeat. Cannot be empty.
    pub target_worker_ids: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AdminFanOutSubmissionResponse {
    pub submissions: Vec<SubmissionResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkRejudgeResponse {
    #[schema(example = 1234)]
    pub queued: usize,
}

pub fn validate_bulk_rejudge(req: &BulkRejudgeRequest) -> Result<(), AppError> {
    if req.submission_ids.is_empty() {
        return Err(AppError::Validation(
            "submission_ids cannot be empty".into(),
        ));
    }

    if let Some(invalid_id) = req.submission_ids.iter().copied().find(|id| *id <= 0) {
        return Err(AppError::Validation(format!(
            "submission_ids contains invalid id {invalid_id}. IDs must be positive integers."
        )));
    }

    if let Some(ref tw) = req.target_worker_id
        && !tw.is_empty()
    {
        validate_worker_id_format(tw)?;
    }

    Ok(())
}

/// Format-only validator for `target_worker_id` strings arriving from API
/// clients. Existence-against-live-heartbeats is checked separately by
/// handlers that have access to `AppState`.
pub fn validate_worker_id_format(id: &str) -> Result<(), AppError> {
    let len = id.len();
    if !(1..=128).contains(&len) {
        return Err(AppError::Validation(
            "target_worker_id must be 1-128 characters".into(),
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(AppError::Validation(
            "target_worker_id may only contain alphanumerics, '-', '_', and '.'".into(),
        ));
    }
    Ok(())
}

const MAX_FAN_OUT_WORKERS: usize = 32;

pub fn validate_admin_fan_out(req: &AdminFanOutSubmissionRequest) -> Result<(), AppError> {
    if req.target_worker_ids.is_empty() {
        return Err(AppError::Validation(
            "target_worker_ids cannot be empty".into(),
        ));
    }
    if req.target_worker_ids.len() > MAX_FAN_OUT_WORKERS {
        return Err(AppError::Validation(format!(
            "target_worker_ids cannot exceed {MAX_FAN_OUT_WORKERS} entries"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for id in &req.target_worker_ids {
        validate_worker_id_format(id)?;
        if !seen.insert(id.as_str()) {
            return Err(AppError::Validation(format!(
                "target_worker_ids contains duplicate '{id}'"
            )));
        }
    }
    Ok(())
}

impl From<crate::entity::test_case_result::Model> for TestCaseResultResponse {
    fn from(m: crate::entity::test_case_result::Model) -> Self {
        Self {
            id: m.id,
            verdict: Some(m.verdict),
            score: Some(m.score),
            time_used: m.time_used,
            memory_used: m.memory_used,
            test_case_id: m.test_case_id,
            input: None,
            expected_output: None,
            stdout: m.stdout,
            stderr: m.stderr,
            checker_output: m.checker_output,
        }
    }
}
