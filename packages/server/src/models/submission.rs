use chrono::{DateTime, Utc};
use common::{SubmissionStatus, Verdict};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

use super::shared::Pagination;
pub use broccoli_api_types::submission::{
    CreateSubmissionRequest, JudgeResultResponse, SubmissionFileDto, SubmissionListItem,
    SubmissionListResponse, SubmissionResponse, TestCaseResultResponse,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionFile {
    pub filename: String,
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

/// Testcase results are always paged; expanded output is fetched for one result only.
#[derive(Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SubmissionResultQuery {
    pub page: Option<u64>,
    /// Maximum 20 results per response.
    pub per_page: Option<u64>,
    /// Optional comma-separated testcase IDs (at most 20), for subtask views.
    pub test_case_ids: Option<String>,
    /// Select one result, e.g. when opening its output dialog.
    pub result_id: Option<i32>,
    /// Larger output preview; requires result_id and still caps each field at 64 KiB.
    #[serde(default)]
    pub full_output: bool,
}

/// The wire shape `apply_submission_judgement`, `rejudge_submission`, and
/// `admin_fan_out_submission` actually return. All three authorise their
/// *mutation* independently of the caller's own Read visibility into the
/// submission (`submission:rejudge`/`system:admin`, never
/// `submission:view_all`), then route the response through
/// `apply_filter_to_response_after_mutation` (`handlers/submission/filter.rs`),
/// which runs the same `Resource::Submission` Read decision `GET
/// /submissions/{id}` would make for that caller:
///
/// - `Allow`: every field below is present, identical to `SubmissionResponse`.
/// - `Redact`: only the fields the decision nominates come back `null`/`[]`.
/// - `Deny`: the mutation still went through, but the response collapses to
///   just `id` - every other field below is entirely ABSENT from the JSON
///   object (not `null` - omitted), because failing the request outright
///   would misrepresent a mutation that did happen. See the `rjv_*` tests in
///   `tests/integration/rejudge_visibility.rs`.
///
/// Every field but `id` is therefore optional here, unlike
/// `SubmissionResponse` (used by `GET /submissions/{id}`, where a `Deny`
/// degrades to a 404 instead of a sparse body, so its fields stay required).
#[derive(Serialize, utoipa::ToSchema)]
pub struct SubmissionResponseAfterMutation {
    #[schema(example = 1)]
    pub id: i32,
    pub files: Option<Vec<SubmissionFileDto>>,
    #[schema(example = "cpp")]
    pub language: Option<String>,
    pub status: Option<SubmissionStatus>,
    #[schema(example = 1)]
    pub user_id: Option<i32>,
    #[schema(example = "alice")]
    pub username: Option<String>,
    #[schema(example = 1)]
    pub problem_id: Option<i32>,
    #[schema(example = "Two Sum")]
    pub problem_title: Option<String>,
    #[schema(example = 1)]
    pub contest_id: Option<i32>,
    #[schema(example = "ioi")]
    pub contest_type: Option<String>,
    #[schema(example = 0)]
    pub judge_epoch: Option<i32>,
    /// When set, the submission has been pinned to this worker by an admin
    /// and every operation it produces will run there.
    #[schema(example = "worker-1")]
    pub target_worker_id: Option<String>,
    #[schema(example = "2025-10-01T14:30:00Z")]
    pub created_at: Option<DateTime<Utc>>,
    pub result: Option<JudgeResultResponse>,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubmissionJudgementResponse {
    /// Number of testcase result changes relative to the current version (admins only).
    #[serde(default)]
    pub case_changes: Option<u64>,
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
    #[serde(default)]
    pub test_case_pagination: Option<Pagination>,
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
    pub submissions: Vec<SubmissionResponseAfterMutation>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkRejudgeResponse {
    /// Count only - never per-id detail, never submission content. See
    /// `bulk_rejudge_submissions`'s doc comment (`handlers/submission/rejudge.rs`)
    /// for exactly what this number does and does not disclose.
    #[schema(example = 1234)]
    pub queued: usize,
}

/// Most ids one `POST /submissions/bulk-rejudge` accepts. The endpoint's
/// OpenAPI description is built from this constant, so the documented and
/// the enforced cap cannot drift. It also keeps the `IN (...)` id list far
/// below Postgres' 65,535 bind-parameter limit.
pub const MAX_BULK_REJUDGE_IDS: usize = 10_000;

pub fn validate_bulk_rejudge(req: &BulkRejudgeRequest) -> Result<(), AppError> {
    if req.submission_ids.is_empty() {
        return Err(AppError::Validation(
            "submission_ids cannot be empty".into(),
        ));
    }

    if req.submission_ids.len() > MAX_BULK_REJUDGE_IDS {
        return Err(AppError::Validation(format!(
            "submission_ids contains {} ids; at most {MAX_BULK_REJUDGE_IDS} are allowed per request",
            req.submission_ids.len()
        )));
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
