use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use broccoli_server_sdk::permissions as perm;
use common::{DlqMessageType, SubmissionStatus};
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use tracing::{info, instrument, warn};

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
use crate::entity::{dead_letter_message, judgement_reset::ClearJudgementColumns, submission};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::handlers::submission::open_rejudge_judgement;
use crate::models::dlq::*;
use crate::models::shared::Pagination;
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "",
    tag = "Dead Letter Queue",
    operation_id = "listDlqMessages",
    summary = "List dead letter messages",
    description = "Returns a paginated list of dead letter messages. Requires `dlq:manage` permission.",
    params(ListDlqParams),
    responses(
        (status = 200, description = "List of DLQ messages", body = DlqListResponse),
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

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).clamp(1, 100);

    let dlq = dlq_service(&state.db);
    let (messages, total) = dlq
        .list(message_type, params.resolved, page, per_page)
        .await?;

    let data: Vec<DlqMessageResponse> = messages.into_iter().map(Into::into).collect();
    let total_pages = total.div_ceil(per_page);

    Ok(Json(DlqListResponse {
        data,
        pagination: Pagination {
            page,
            per_page,
            total,
            total_pages,
        },
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

/// Re-queue one `stuck_submission` DLQ message for judging, in its OWN
/// transaction.
///
/// Owning the transaction per message is what makes the bulk path correct.
/// Postgres aborts a whole transaction on the first statement error (or on a
/// deadlock against the deferred-judgement steal scan, which locks the same
/// (submission, judgement) pair in the opposite order): every later statement
/// then fails with `25P02 current transaction is aborted` and the final
/// `COMMIT` silently rolls back. A `continue`-on-error loop over a *shared*
/// batch transaction therefore discards every successful sibling in the batch
/// and loses the retried/skipped/errors accounting. A per-message transaction
/// contains any failure to that one message; the caller reports it and the rest
/// of the batch proceeds.
///
/// The lineage preparation mirrors the admin rejudge path: lock the submission
/// `FOR UPDATE`, demote the stale finalized judgement and insert a fresh
/// `is_current` one at a bumped epoch via `open_rejudge_judgement`, reset the
/// submission to `Queued` with its judged output cleared, and resolve the DLQ
/// message. The claim fiber (`dispatcher/claim.rs`) promotes `Queued` to
/// `Pending` and dispatches - no handler-side spawn, so an api crash between
/// commit and dispatch cannot lose the retry.
async fn retry_stuck_submission(
    db: &DatabaseConnection,
    message_id: i32,
    admin_user_id: i32,
) -> Result<StuckRetryOutcome, AppError> {
    let txn = db.begin().await?;
    let dlq = DlqService::new(&txn);

    let message = dlq
        .get_by_id_for_update(message_id)
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

    // Lock the submission FOR UPDATE, mirroring the admin rejudge handlers:
    // it serializes this retry against a concurrent rejudge/steal on the same
    // submission so the judgement-lineage demote+insert below can't race into
    // a duplicate `is_current` row.
    let Some(sub) = submission::Entity::find_by_id(submission_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
    else {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::SubmissionMissing { submission_id },
        ));
    };

    if sub.status != SubmissionStatus::SystemError && sub.status != SubmissionStatus::Pending {
        return Ok(StuckRetryOutcome::Skipped(
            StuckRetrySkip::NotRetryableStatus {
                submission_id,
                status: sub.status,
            },
        ));
    }

    // A DLQ retry is an immediate rejudge: prepare the judgement lineage
    // exactly as the admin rejudge path (`open_rejudge_judgement`) does.
    // Without this, the still-`is_current`, now-finalized judgement left by
    // the SystemError terminalization stays current; the claim fiber's
    // `ensure_active_judgement_id` then finds no non-finalized current row,
    // tries to INSERT a fresh `is_current=true` judgement, and collides with
    // the partial unique index `idx_submission_judgement_one_current` - the
    // insert error is swallowed and the submission dispatches with
    // judgement id=0 (all judgement writes no-op; the versioned judgement is
    // stranded at SystemError). Demoting the stale judgement and inserting a
    // fresh one at a bumped epoch closes that.
    let new_epoch = sub.judge_epoch.saturating_add(1);
    open_rejudge_judgement(
        &txn,
        &sub,
        admin_user_id,
        sub.target_worker_id.clone(),
        None,
        new_epoch,
        true, // apply_immediately: the retried judgement is the displayed verdict
    )
    .await?;

    // Reset the submission into the durable-accept (`Queued`) state at the
    // new epoch. The claim fiber promotes it to `Pending` and re-dispatches,
    // adopting the fresh judgement inserted above. `clear_judgement_columns`
    // NULLs every judged-output column (including error_code/error_message)
    // from the single source of truth in `entity::judgement_reset`.
    submission::Entity::update_many()
        .clear_judgement_columns()
        .col_expr(
            submission::Column::Status,
            Expr::value(SubmissionStatus::Queued.to_string()),
        )
        .col_expr(submission::Column::JudgeEpoch, Expr::value(new_epoch))
        .filter(submission::Column::Id.eq(submission_id))
        .exec(&txn)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to reset submission status: {}", e)))?;

    match dlq.resolve(message_id, Some(admin_user_id)).await? {
        ResolveResult::Resolved => {}
        ResolveResult::AlreadyResolved => {
            warn!(
                id = message_id,
                "DLQ message was resolved concurrently during retry"
            );
        }
        ResolveResult::NotFound => {
            return Err(AppError::Internal(
                "DLQ message disappeared during retry".into(),
            ));
        }
    }

    txn.commit().await?;

    Ok(StuckRetryOutcome::Requeued { submission_id })
}

/// Map an expected retry-skip condition to the single-message endpoint's HTTP
/// error. The bulk endpoint tallies skips instead of erroring, so this mapping
/// is intentionally only used by `retry_dlq_message`.
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
            "Submission {} is in '{}' state and cannot be retried. Only SystemError or Pending submissions can be retried.",
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
    description = "Retries a dead letter message by resetting the submission to Queued (the claim fiber then promotes it to Pending) and re-dispatching it to the plugin-based judging system. Only stuck_submission messages can be retried; operation_task, stuck_code_run, and stuck_submission_judgement messages are visibility-only from here. Marks the DLQ entry as resolved. Requires `dlq:manage` permission.",
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
    // UP#39 backpressure-on-post: DLQ retry flips the submission back
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
    let result = dlq.resolve(id, Some(auth_user.user_id)).await?;

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
    description = "Retries multiple dead letter messages by resetting their submissions to Queued (the claim fiber then promotes each to Pending) and re-dispatching to the plugin-based judging system. Supports either specific message IDs or filter-based selection. Only stuck_submission messages with a known submission_id in SystemError or Pending state are retryable; other message types are skipped. Requires `dlq:manage` permission.",
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
    // UP#39 backpressure-on-post: bulk DLQ retry flips many
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

    // UP#37: each reset submission is now in `Queued`; the claim fiber
    // (UP#38, `dispatcher/claim.rs`) takes it from there. We no longer
    // reload + spawn per row - that path was the very silent-loss
    // vector this PR closes.

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
        .resolve_many(&payload.message_ids, Some(auth_user.user_id))
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
