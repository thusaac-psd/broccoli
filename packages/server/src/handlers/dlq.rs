use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use broccoli_server_sdk::permissions as perm;
use common::db::DatabaseConnection;
use common::{DlqMessageType, SubmissionStatus};
use sea_orm::sea_query::LockType;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};
use tracing::{info, instrument};

use crate::dispatcher::queue_depth::enforce_queue_depth_admission;
use crate::dlq::{DlqService, ResolveResult, dlq_service};
// visibility-bypass-audited: every handler in this module requires
// perm::DLQ_MANAGE, pinned by tests/integration/dlq.rs's
// `contestant_cannot_list_dlq`, `contestant_cannot_get_stats`,
// `contestant_cannot_get_dlq_detail`, `contestant_cannot_resolve_dlq_message`,
// `contestant_cannot_retry_dlq_message`, `contestant_cannot_bulk_retry`, and
// `contestant_cannot_bulk_delete`. This is judge-operator tooling over the
// dead letter queue, never a per-row Contest/Problem/Submission view the
// kernel governs.
use crate::entity::{dead_letter_message, submission};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::dlq::*;
use crate::models::shared::PageWindow;
use crate::services::judging::{OpenJudgement, open_judgement};
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "",
    tag = "Dead Letter Queue",
    operation_id = "listDlqMessages",
    summary = "List dead letter messages",
    description = "Returns a paginated list of dead letter messages, most recently failed first. Requires `dlq:manage` permission.",
    params(ListDlqParams),
    responses(
        (status = 200, description = "List of DLQ messages", body = DlqListResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn list_dlq_messages(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<ListDlqParams>,
) -> Result<Json<DlqListResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;

    let message_type = params
        .message_type
        .map(|mt| mt.parse::<DlqMessageType>())
        .transpose()
        .map_err(AppError::Validation)?;

    let window = PageWindow::new(params.page, params.per_page)?;

    let dlq = dlq_service(&state.db);
    let (messages, total) = dlq.list(message_type, params.resolved, window).await?;

    let data: Vec<DlqMessageResponse> = messages.into_iter().map(Into::into).collect();

    Ok(Json(DlqListResponse {
        data,
        pagination: window.pagination(total),
    }))
}

#[utoipa::path(
    get,
    path = "/stats",
    tag = "Dead Letter Queue",
    operation_id = "getDlqStats",
    summary = "Get DLQ statistics",
    description = "Returns statistics about the dead letter queue. Requires `dlq:manage` permission.",
    responses(
        (status = 200, description = "DLQ statistics", body = DlqStatsResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn get_dlq_stats(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<DlqStatsResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;

    let dlq = dlq_service(&state.db);
    let stats = dlq.stats().await?;

    Ok(Json(stats.into()))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Dead Letter Queue",
    operation_id = "getDlqMessage",
    summary = "Get DLQ message details",
    description = "Returns full details of a DLQ message including payload and retry history. Requires `dlq:manage` permission.",
    params(("id" = i32, Path, description = "DLQ message ID")),
    responses(
        (status = 200, description = "DLQ message details", body = DlqMessageDetailResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Message not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_dlq_message(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<DlqMessageDetailResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;

    let dlq = dlq_service(&state.db);
    let message = dlq
        .get_by_id(id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("DLQ message {} not found", id)))?;

    Ok(Json(message.into()))
}

/// Why a stuck-submission retry did not re-queue its submission.
///
/// These are *expected*, per-message conditions (as opposed to a hard DB
/// failure): the single-message endpoint maps each to a specific 4xx, and the
/// bulk endpoint tallies them as `skipped` - except `SubmissionMissing`, which
/// the bulk path reports as an error to preserve its prior behaviour.
enum StuckRetrySkip {
    AlreadyResolved,
    NotStuckSubmission,
    UnknownSubmission,
    SubmissionMissing {
        submission_id: i32,
    },
    NotRetryableStatus {
        submission_id: i32,
        status: SubmissionStatus,
    },
}

/// Outcome of `retry_stuck_submission` for a single DLQ message.
enum StuckRetryOutcome {
    Requeued { submission_id: i32 },
    Skipped(StuckRetrySkip),
}

/// Re-queue one `stuck_submission` DLQ message for judging, in its own
/// transaction, so one failing message in a bulk retry cannot abort its
/// siblings.
///
/// Locks follow the judging order (submission, then judgement, then dead
/// letter): the message is read unlocked to find its submission, the
/// submission is locked, and only then is the message locked and re-checked.
/// The retry is an immediate rejudge (`open_judgement`), which the claim
/// fiber dispatches. The message stays open with the retry recorded in its
/// history; the judging lifecycle resolves it once the rejudge ends with a
/// verdict that stands, and a retry that ends as a system error again leaves
/// it open.
async fn retry_stuck_submission(
    db: &DatabaseConnection,
    message_id: i32,
    admin_user_id: i32,
) -> Result<StuckRetryOutcome, AppError> {
    let txn = db.begin().await?;
    let dlq = DlqService::new(&txn);

    let message = dlq
        .get_by_id(message_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("DLQ message {} not found", message_id)))?;
    if message.resolved {
        return Ok(StuckRetryOutcome::Skipped(StuckRetrySkip::AlreadyResolved));
    }
    if message.message_type != DlqMessageType::StuckSubmission.as_str() {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::NotStuckSubmission,
        ));
    }
    let Some(submission_id) = message.submission_id else {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::UnknownSubmission,
        ));
    };

    let Some(sub) = submission::Entity::find_by_id(submission_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
    else {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::SubmissionMissing { submission_id },
        ));
    };
    let message = dlq
        .get_by_id_for_update(message_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("DLQ message {} not found", message_id)))?;
    if message.resolved {
        return Ok(StuckRetryOutcome::Skipped(StuckRetrySkip::AlreadyResolved));
    }

    // Only a submission whose verdict is a SystemError is retried; one that
    // is queued, in flight or judged is already being handled.
    if sub.status != SubmissionStatus::SystemError {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::NotRetryableStatus {
                submission_id,
                status: sub.status,
            },
        ));
    }

    open_judgement(
        &txn,
        sub.id,
        OpenJudgement::Rejudge {
            triggered_by_user_id: admin_user_id,
            target_worker_id: None,
        },
    )
    .await?;
    dlq.record_retry(message_id, admin_user_id).await?;
    txn.commit().await?;

    Ok(StuckRetryOutcome::Requeued { submission_id })
}

fn stuck_retry_skip_to_error(skip: StuckRetrySkip) -> AppError {
    match skip {
        StuckRetrySkip::AlreadyResolved => AppError::Conflict("Message already resolved".into()),
        StuckRetrySkip::NotStuckSubmission => AppError::Validation(
            "Only stuck_submission messages can be retried. operation_task, stuck_code_run, and stuck_submission_judgement messages are visibility-only from this endpoint.".into(),
        ),
        StuckRetrySkip::UnknownSubmission => AppError::Validation(
            "Cannot retry: submission_id is unknown (message had deserialization failure)".into(),
        ),
        StuckRetrySkip::SubmissionMissing { submission_id } => {
            AppError::NotFound(format!("Submission {} not found", submission_id))
        }
        StuckRetrySkip::NotRetryableStatus {
            submission_id,
            status,
        } => AppError::Validation(format!(
            "Submission {} is in '{}' state and cannot be retried. Only SystemError submissions can be retried.",
            submission_id, status
        )),
    }
}

#[utoipa::path(
    post,
    path = "/{id}/retry",
    tag = "Dead Letter Queue",
    operation_id = "retryDlqMessage",
    summary = "Retry a DLQ message",
    description = "Retries a dead letter message by resetting the submission to Queued (the claim fiber then promotes it to Pending) and re-dispatching it to the plugin-based judging system. Only stuck_submission messages can be retried; operation_task, stuck_code_run, and stuck_submission_judgement messages are visibility-only from here. The message stays unresolved with the retry recorded in its history until the rejudge ends with a verdict other than a system error, which resolves it. Requires `dlq:manage` permission.",
    params(("id" = i32, Path, description = "DLQ message ID")),
    responses(
        (status = 200, description = "Submission re-dispatched", body = DlqRetryResponse),
        (status = 400, description = "Only stuck_submission messages can be retried, or submission is not in a retryable state (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Message or submission not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Message already resolved (CONFLICT)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn retry_dlq_message(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<DlqRetryResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;
    // Backpressure on POST: DLQ retry flips the submission back
    // into `Queued` (see `retry_stuck_submission`). Treat it as a
    // fresh durable-accept and apply the same cap.
    enforce_queue_depth_admission(&state).await?;

    match retry_stuck_submission(&state.db, id, auth_user.user_id).await? {
        StuckRetryOutcome::Requeued { submission_id } => {
            info!(
                id,
                submission_id, "DLQ message reset to Queued for claim-fiber dispatch"
            );
            Ok(Json(DlqRetryResponse {
                message: format!("Submission {} re-queued for judging", submission_id),
            }))
        }
        StuckRetryOutcome::Skipped(skip) => Err(stuck_retry_skip_to_error(skip)),
    }
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Dead Letter Queue",
    operation_id = "deleteDlqMessage",
    summary = "Delete (resolve) a DLQ message",
    description = "Marks a DLQ message as resolved without retrying. Use this to acknowledge messages that don't need to be reprocessed. Requires `dlq:manage` permission.",
    params(("id" = i32, Path, description = "DLQ message ID")),
    responses(
        (status = 204, description = "Message resolved"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Message not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn delete_dlq_message(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;

    let dlq = dlq_service(&state.db);
    let result = dlq.resolve(id, auth_user.user_id).await?;

    match result {
        ResolveResult::Resolved => {
            info!(id, "DLQ message resolved");
            Ok(StatusCode::NO_CONTENT)
        }
        ResolveResult::NotFound => Err(AppError::NotFound(format!("DLQ message {} not found", id))),
        ResolveResult::AlreadyResolved => {
            info!(id, "DLQ message already resolved");
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

#[utoipa::path(
    post,
    path = "/bulk-retry",
    tag = "Dead Letter Queue",
    operation_id = "bulkRetryDlq",
    summary = "Bulk-retry DLQ messages",
    description = "Retries multiple dead letter messages by resetting their submissions to Queued (the claim fiber then promotes each to Pending) and re-dispatching to the plugin-based judging system. Supports either specific message IDs or filter-based selection. Only stuck_submission messages with a known submission_id in SystemError state are retryable; other message types are skipped. Requires `dlq:manage` permission.",
    request_body = BulkRetryDlqRequest,
    responses(
        (status = 200, description = "Bulk retry result", body = BulkRetryDlqResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn bulk_retry_dlq(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<BulkRetryDlqRequest>,
) -> Result<Json<BulkRetryDlqResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;
    validate_bulk_retry_dlq(&payload)?;
    // Backpressure on POST: bulk DLQ retry flips many
    // submissions back to `Queued` in a single call. The check here
    // samples the depth **once at the start** of the bulk insert,
    // which is intentionally permissive: the cap is a steady-state
    // circuit-breaker, not a strict per-row gate. A DLQ retry storm
    // that arrives just below the cap can take the depth meaningfully
    // past it for the rest of that call's duration; the next caller
    // (or the next bulk-retry) will be rejected, restoring
    // equilibrium. The alternative - per-chunk recheck - would
    // serialize the bulk path into hundreds of COUNT round-trips and
    // defeat the bulk endpoint, so we accept bounded overshoot under
    // a retry storm in exchange for keeping bulk retry a single
    // logical operation. Mirrors `bulk_rejudge_submissions`
    // (`submission.rs`) by design.
    enforce_queue_depth_admission(&state).await?;

    let message_ids: Vec<i32> = if let Some(ref ids) = payload.message_ids {
        ids.clone()
    } else {
        let mut query = dead_letter_message::Entity::find()
            .filter(dead_letter_message::Column::Resolved.eq(false));

        if let Some(ref mt) = payload.message_type {
            query = query.filter(dead_letter_message::Column::MessageType.eq(mt.as_str()));
        }
        if let Some(ref ec) = payload.error_code {
            query = query.filter(dead_letter_message::Column::ErrorCode.eq(ec.as_str()));
        }

        let ids: Vec<i32> = query
            .select_only()
            .column(dead_letter_message::Column::Id)
            .order_by_asc(dead_letter_message::Column::CreatedAt)
            .limit(10001)
            .into_tuple::<i32>()
            .all(&state.db)
            .await?;

        if ids.len() > 10_000 {
            return Err(AppError::Validation(
                "Filter matches more than 10,000 messages. Narrow your filters.".into(),
            ));
        }

        ids
    };

    let mut retried = 0usize;
    let mut skipped = 0usize;
    let mut errors = Vec::new();
    let mut submissions_to_dispatch: Vec<i32> = Vec::new();

    // Each message is retried in its OWN transaction (see
    // `retry_stuck_submission`). A shared per-batch transaction is aborted
    // wholesale by Postgres on the first failing message - a statement error,
    // or a deadlock against the deferred-judgement steal scan - after which
    // every later statement returns `25P02 current transaction is aborted` and
    // the final commit silently rolls back. That would discard every
    // successful sibling in the batch and lose the retried/skipped/errors
    // accounting. Per-message isolation keeps the partial-success tally honest
    // and contains any failure (or deadlock) to the single offending message.
    for id in &message_ids {
        match retry_stuck_submission(&state.db, *id, auth_user.user_id).await {
            Ok(StuckRetryOutcome::Requeued { submission_id }) => {
                submissions_to_dispatch.push(submission_id);
                retried += 1;
            }
            // A missing submission is reported as an error to match the prior
            // bulk behaviour; every other skip is an expected, non-retryable
            // condition tallied as `skipped`.
            Ok(StuckRetryOutcome::Skipped(StuckRetrySkip::SubmissionMissing { submission_id })) => {
                errors.push(BulkRetryError {
                    id: *id,
                    error: format!("Submission {} not found", submission_id),
                });
            }
            Ok(StuckRetryOutcome::Skipped(_)) => {
                skipped += 1;
            }
            Err(e) => {
                errors.push(BulkRetryError {
                    id: *id,
                    error: format!("{e:?}"),
                });
            }
        }
    }

    // Each reset submission is now in `Queued`; the claim fiber
    // (`dispatcher/claim.rs`) takes it from there. Do not reload and
    // spawn a dispatch per row here: a crash between commit and spawn
    // would silently lose the retry.

    info!(
        retried,
        skipped,
        errors = errors.len(),
        user_id = auth_user.user_id,
        queued_for_claim = submissions_to_dispatch.len(),
        "Bulk retried DLQ messages reset to Queued for claim-fiber dispatch"
    );

    Ok(Json(BulkRetryDlqResponse {
        retried,
        skipped,
        errors,
    }))
}

#[utoipa::path(
    delete,
    path = "/bulk",
    tag = "Dead Letter Queue",
    operation_id = "bulkDeleteDlq",
    summary = "Bulk-delete (resolve) DLQ messages",
    description = "Marks multiple DLQ messages as resolved without retrying. Requires `dlq:manage` permission.",
    request_body = BulkDeleteDlqRequest,
    responses(
        (status = 200, description = "Messages resolved", body = BulkDeleteDlqResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn bulk_delete_dlq(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<BulkDeleteDlqRequest>,
) -> Result<Json<BulkDeleteDlqResponse>, AppError> {
    auth_user.require_permission(perm::DLQ_MANAGE)?;
    validate_bulk_delete_dlq(&payload)?;

    let dlq = dlq_service(&state.db);
    let rows_affected = dlq
        .resolve_many(&payload.message_ids, auth_user.user_id)
        .await?;

    info!(
        deleted = rows_affected,
        user_id = auth_user.user_id,
        "Bulk resolved DLQ messages"
    );

    Ok(Json(BulkDeleteDlqResponse {
        deleted: rows_affected as usize,
    }))
}
