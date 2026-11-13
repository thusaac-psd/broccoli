use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dlq::DlqStats;
use crate::entity::dead_letter_message;
use crate::error::AppError;

use super::shared::{Pagination, validate_bulk_ids};
pub use broccoli_api_types::dlq::{DlqStatsResponse, MessageTypeCounts};

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListDlqParams {
    #[param(example = "stuck_submission")]
    pub message_type: Option<String>,
    #[param(example = false)]
    pub resolved: Option<bool>,
    #[param(example = 1)]
    pub page: Option<u64>,
    #[param(example = 20)]
    pub per_page: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DlqMessageResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "job-abc123")]
    pub message_id: String,
    #[schema(example = "stuck_submission")]
    pub message_type: String,
    #[schema(example = 42)]
    pub submission_id: Option<i32>,
    #[schema(example = "MAX_RETRIES_EXCEEDED")]
    pub error_code: String,
    #[schema(example = "Database connection timeout")]
    pub error_message: String,
    /// Attempts recorded in `retry_history`: failed deliveries of an operation
    /// task, or each dead-lettering and operator retry of stuck work.
    #[schema(example = 3)]
    pub retry_count: i32,
    #[schema(example = "2025-09-01T08:00:00Z")]
    pub first_failed_at: DateTime<Utc>,
    #[schema(example = "2025-09-01T08:05:00Z")]
    pub created_at: DateTime<Utc>,
    /// When the message was last dead-lettered; a recurrence reopens the
    /// message and moves this forward. Lists sort by it, newest first.
    #[schema(example = "2025-09-02T10:00:00Z")]
    pub last_failed_at: DateTime<Utc>,
    /// Resolved by an operator, or, for a stuck submission, automatically once
    /// it finished with a verdict other than a system error.
    #[schema(example = false)]
    pub resolved: bool,
    pub resolved_at: Option<DateTime<Utc>>,
    /// The operator who resolved it; null when it was resolved automatically.
    pub resolved_by: Option<i32>,
}

impl From<dead_letter_message::Model> for DlqMessageResponse {
    fn from(m: dead_letter_message::Model) -> Self {
        Self {
            id: m.id,
            message_id: m.message_id,
            message_type: m.message_type,
            submission_id: m.submission_id,
            error_code: m.error_code,
            error_message: m.error_message,
            retry_count: m.retry_count,
            first_failed_at: m.first_failed_at,
            created_at: m.created_at,
            last_failed_at: m.last_failed_at,
            resolved: m.resolved,
            resolved_at: m.resolved_at,
            resolved_by: m.resolved_by,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DlqMessageDetailResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "job-abc123")]
    pub message_id: String,
    #[schema(example = "stuck_submission")]
    pub message_type: String,
    #[schema(example = 42)]
    pub submission_id: Option<i32>,
    pub payload: serde_json::Value,
    #[schema(example = "MAX_RETRIES_EXCEEDED")]
    pub error_code: String,
    #[schema(example = "Database connection timeout")]
    pub error_message: String,
    /// Attempts recorded in `retry_history`: failed deliveries of an operation
    /// task, or each dead-lettering and operator retry of stuck work.
    #[schema(example = 3)]
    pub retry_count: i32,
    pub retry_history: serde_json::Value,
    #[schema(example = "2025-09-01T08:00:00Z")]
    pub first_failed_at: DateTime<Utc>,
    #[schema(example = "2025-09-01T08:05:00Z")]
    pub created_at: DateTime<Utc>,
    /// When the message was last dead-lettered; a recurrence reopens the
    /// message and moves this forward. Lists sort by it, newest first.
    #[schema(example = "2025-09-02T10:00:00Z")]
    pub last_failed_at: DateTime<Utc>,
    /// Resolved by an operator, or, for a stuck submission, automatically once
    /// it finished with a verdict other than a system error.
    #[schema(example = false)]
    pub resolved: bool,
    pub resolved_at: Option<DateTime<Utc>>,
    /// The operator who resolved it; null when it was resolved automatically.
    pub resolved_by: Option<i32>,
}

impl From<dead_letter_message::Model> for DlqMessageDetailResponse {
    fn from(m: dead_letter_message::Model) -> Self {
        Self {
            id: m.id,
            message_id: m.message_id,
            message_type: m.message_type,
            submission_id: m.submission_id,
            payload: m.payload,
            error_code: m.error_code,
            error_message: m.error_message,
            retry_count: m.retry_count,
            retry_history: m.retry_history,
            first_failed_at: m.first_failed_at,
            created_at: m.created_at,
            last_failed_at: m.last_failed_at,
            resolved: m.resolved,
            resolved_at: m.resolved_at,
            resolved_by: m.resolved_by,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DlqListResponse {
    pub data: Vec<DlqMessageResponse>,
    pub pagination: Pagination,
}

impl From<DlqStats> for DlqStatsResponse {
    fn from(s: DlqStats) -> Self {
        Self {
            total_unresolved: s.total_unresolved,
            total_resolved: s.total_resolved,
            unresolved_by_message_type: MessageTypeCounts {
                operation_task: s.operation_task_count,
                stuck_submission: s.stuck_submission_count,
                stuck_code_run: s.stuck_code_run_count,
                stuck_submission_judgement: s.stuck_submission_judgement_count,
            },
            unresolved_by_error_code: s.unresolved_by_error_code,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DlqRetryResponse {
    #[schema(example = "Message requeued for processing")]
    pub message: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkRetryDlqRequest {
    pub message_ids: Option<Vec<i32>>,
    pub message_type: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkRetryDlqResponse {
    #[schema(example = 2)]
    pub retried: usize,
    #[schema(example = 1)]
    pub skipped: usize,
    pub errors: Vec<BulkRetryError>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkRetryError {
    #[schema(example = 7)]
    pub id: i32,
    #[schema(example = "Message not found")]
    pub error: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkDeleteDlqRequest {
    #[schema(example = json!([5, 7, 9]))]
    pub message_ids: Vec<i32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkDeleteDlqResponse {
    #[schema(example = 3)]
    pub deleted: usize,
}

pub fn validate_bulk_retry_dlq(req: &BulkRetryDlqRequest) -> Result<(), AppError> {
    let has_ids = req.message_ids.as_ref().is_some_and(|ids| !ids.is_empty());
    let has_filter = req.message_type.is_some() || req.error_code.is_some();

    if !has_ids && !has_filter {
        return Err(AppError::Validation(
            "Either 'message_ids' or filter fields ('message_type'/'error_code') must be provided"
                .into(),
        ));
    }
    if has_ids && has_filter {
        return Err(AppError::Validation(
            "Cannot combine 'message_ids' with filter fields".into(),
        ));
    }

    if let Some(ref ids) = req.message_ids {
        validate_bulk_ids(ids, "message_ids", 1000)?;
    }

    Ok(())
}

pub fn validate_bulk_delete_dlq(req: &BulkDeleteDlqRequest) -> Result<(), AppError> {
    validate_bulk_ids(&req.message_ids, "message_ids", 1000)
}
