use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::sea_query::LockType;
use sea_orm::*;
use serde::Deserialize;
use tracing::{info, instrument};

use crate::dispatcher::queue_depth::enforce_queue_depth_admission;
// visibility-bypass-audited: every handler in this module requires
// perm::SUBMISSION_REJUDGE at entry (the `target_worker_id` override further
// requires perm::SYSTEM_ADMIN), pinned by `contestant_cannot_rejudge` and
// `contestant_cannot_bulk_rejudge` (tests/integration/submission.rs). This is
// system/judge-operator tooling that mutates submissions, not a viewer read
// path - but the `SubmissionResponse` it builds is still routed through
// `VisibilityKernel::decide`/`fetch_visible` before it ships (see
// `apply_filter_to_response_after_mutation`), because `submission:rejudge`/
// `system:admin` are independent of `Resource::Submission`'s own Read
// reachability: an operator can trigger a rejudge on a submission they could
// not themselves `GET` (not the owner, no `submission:view_all`, not a
// contest participant), and a frozen ICPC contest / restricted IOI feedback
// level must redact this response exactly as it would that `GET`. Only the
// *mutation* is authorised by `submission:rejudge` alone; what comes back
// reflects the actor's own Read visibility, not a blanket admin view. Pinned
// by `tests/integration/rejudge_visibility.rs`.
use crate::entity::{
    judgement_reset::ClearJudgementActiveModel, submission, submission_judgement, test_case_result,
};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::FreshAuthUser;
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::submission::*;
use crate::services::submission_dispatch::fire_after_judging_hooks;
use crate::state::AppState;
use crate::utils::contest::{find_contest, is_problem_in_contest};
use crate::utils::judging::{files_to_json, validate_code_payload, validate_submission_contract};
use crate::utils::problem::find_problem;
use crate::visibility::{Subject, VisibilityKernel};

use super::dispatch::open_rejudge_judgement;
use super::filter::apply_filter_to_response_after_mutation;
use super::response::{VisibilityContext, build_submission_response};

#[utoipa::path(
    post,
    path = "/{id}/judgements/{judgement_id}/apply",
    tag = "Submissions",
    operation_id = "applySubmissionJudgement",
    summary = "Apply a finalized submission judgement",
    description = "Makes a finalized judgement the current version and copies its cached result fields onto the submission. Requires `submission:rejudge` permission.",
    params(
        ("id" = i32, Path, description = "Submission ID"),
        ("judgement_id" = i32, Path, description = "Judgement ID")
    ),
    responses(
        (status = 200, description = "Applied judgement", body = SubmissionResponse),
        (status = 400, description = "Judgement is not finalized (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission or judgement not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(submission_id = %id, judgement_id = %judgement_id))]
pub async fn apply_submission_judgement(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((id, judgement_id)): AppPath<(i32, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::SUBMISSION_REJUDGE)?;

    let txn = state.db.begin().await?;
    let sub = submission::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))?;

    let judgement = submission_judgement::Entity::find_by_id(judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Judgement not found".into()))?;

    if !judgement.is_finalized {
        return Err(AppError::Validation(
            "Cannot apply a judgement that is not finalized".into(),
        ));
    }

    submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::IsCurrent,
            sea_orm::sea_query::Expr::value(false),
        )
        .filter(submission_judgement::Column::SubmissionId.eq(id))
        .exec(&txn)
        .await?;

    let mut active_judgement: submission_judgement::ActiveModel = judgement.clone().into();
    active_judgement.is_current = Set(true);
    active_judgement.update(&txn).await?;

    let mut active_submission: submission::ActiveModel = sub.into();
    active_submission.status = Set(judgement.status);
    active_submission.verdict = Set(judgement.verdict);
    active_submission.compile_output = Set(judgement.compile_output);
    active_submission.error_code = Set(judgement.error_code);
    active_submission.error_message = Set(judgement.error_message);
    active_submission.score = Set(judgement.score);
    active_submission.time_used = Set(judgement.time_used);
    active_submission.memory_used = Set(judgement.memory_used);
    active_submission.judged_at = Set(judgement.finalized_at);
    active_submission.judge_epoch = Set(judgement.judge_epoch);
    active_submission.target_worker_id = Set(judgement.target_worker_id);
    let updated = active_submission.update(&txn).await?;
    txn.commit().await?;

    fire_after_judging_hooks(
        &state.db,
        state.registries.hook_registry.clone(),
        updated.id,
        updated.user_id,
        updated.problem_id,
        updated.contest_id,
    )
    .await;

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response =
        build_submission_response(&state.db, &*state.blob_store, updated, visibility).await?;
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let response = apply_filter_to_response_after_mutation(&kernel, response).await?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/{id}/judgements/{judgement_id}/discard",
    tag = "Submissions",
    operation_id = "discardSubmissionJudgement",
    summary = "Discard a non-current submission judgement",
    description = "Deletes a non-current judgement and its test-case result rows. Requires `submission:rejudge` permission.",
    params(
        ("id" = i32, Path, description = "Submission ID"),
        ("judgement_id" = i32, Path, description = "Judgement ID")
    ),
    responses(
        (status = 204, description = "Judgement discarded"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission or judgement not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Cannot discard current judgement (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(submission_id = %id, judgement_id = %judgement_id))]
pub async fn discard_submission_judgement(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((id, judgement_id)): AppPath<(i32, i32)>,
) -> Result<StatusCode, AppError> {
    auth_user.require_permission(perm::SUBMISSION_REJUDGE)?;

    let txn = state.db.begin().await?;
    let _sub = submission::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))?;

    let judgement = submission_judgement::Entity::find_by_id(judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Judgement not found".into()))?;

    if judgement.is_current {
        return Err(AppError::Conflict(
            "Cannot discard the current judgement".into(),
        ));
    }
    if !judgement.is_finalized {
        return Err(AppError::Conflict(
            "Cannot discard a judgement that is still running".into(),
        ));
    }

    test_case_result::Entity::delete_many()
        .filter(test_case_result::Column::JudgementId.eq(Some(judgement_id)))
        .exec(&txn)
        .await?;
    submission_judgement::Entity::delete_by_id(judgement_id)
        .exec(&txn)
        .await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RejudgeQuery {
    #[param(example = "worker-1")]
    pub target_worker_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/{id}/rejudge",
    tag = "Submissions",
    operation_id = "rejudgeSubmission",
    summary = "Rejudge a submission",
    description = "Re-queues the submission for judging. Requires `submission:rejudge` permission. Optionally pin to a worker via `?target_worker_id=...` (requires `system:admin`); pass an empty value to clear an existing pin (also requires `system:admin`).",
    params(
        ("id" = i32, Path, description = "Submission ID"),
        RejudgeQuery,
    ),
    request_body = RejudgeRequest,
    responses(
        (status = 200, description = "Submission re-queued", body = SubmissionResponse),
        (status = 400, description = "Invalid worker (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query), fields(submission_id = %id))]
pub async fn rejudge_submission(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    Query(query): Query<RejudgeQuery>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::SUBMISSION_REJUDGE)?;

    let payload = if body.is_empty() {
        RejudgeRequest::default()
    } else {
        serde_json::from_slice::<RejudgeRequest>(&body)
            .map_err(|e| AppError::Validation(format!("Invalid rejudge request body: {e}")))?
    };
    // UP#39 backpressure-on-post: covers both branches of this
    // handler - `apply_immediately=true` inserts a `Queued` row into
    // `submission`, `apply_immediately=false` inserts one into
    // `submission_judgement`. Both are counted in
    // `count_queued_rows()` so the cap applies uniformly.
    enforce_queue_depth_admission(&state).await?;

    let requested_target = payload
        .target_worker_id
        .as_deref()
        .or(query.target_worker_id.as_deref());
    let new_target: Option<Option<String>> = match requested_target {
        None => None,
        Some("") => {
            auth_user.require_permission(perm::SYSTEM_ADMIN)?;
            Some(None)
        }
        Some(raw) => {
            auth_user.require_permission(perm::SYSTEM_ADMIN)?;
            validate_worker_id_format(raw)?;
            let live = crate::handlers::system::live_worker_ids(&state).await;
            if !live.contains(raw) {
                return Err(AppError::Validation(format!(
                    "Worker '{raw}' has no live heartbeat"
                )));
            }
            Some(Some(raw.to_string()))
        }
    };

    let txn = state.db.begin().await?;

    let sub = submission::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))?;

    let new_epoch = sub.judge_epoch.saturating_add(1);

    // The prior verdict (and its test_case_result rows) stay attached to
    // the old judgement so it remains visible as version history. The
    // dispatch path will look up the new current judgement created here.
    let resolved_target = new_target
        .clone()
        .unwrap_or_else(|| sub.target_worker_id.clone());
    let _new_judgement = open_rejudge_judgement(
        &txn,
        &sub,
        auth_user.user_id,
        resolved_target,
        None,
        new_epoch,
        payload.apply_immediately,
    )
    .await?;

    let updated = if payload.apply_immediately {
        let mut active: submission::ActiveModel = sub.clone().into();
        // UP#37: rejudge that takes effect immediately enters the same
        // durable-accept lifecycle as a fresh POST - flip to `Queued`
        // and let the claim fiber (UP#38) promote to `Pending` and
        // dispatch. The new judgement row was just created above by
        // `open_rejudge_judgement` and is `is_current=true`, so the
        // claim path's call to `ensure_active_judgement_id` will pick
        // it up.
        active.status = Set(SubmissionStatus::Queued);
        active.clear_judgement_columns();
        active.judge_epoch = Set(new_epoch);
        if let Some(target) = new_target.clone() {
            active.target_worker_id = Set(target);
        }
        active.update(&txn).await?
    } else {
        sub.clone()
    };

    txn.commit().await?;

    // UP#37 + residual fix: both branches are durable now.
    //
    // - `apply_immediately=true` flips the submission row to `Queued`;
    //   the claim fiber's submission scan promotes it to Pending and
    //   dispatches.
    // - `apply_immediately=false` inserts a non-current judgement at
    //   `status=Queued` (see `open_rejudge_judgement` above); the claim
    //   fiber's judgement scan picks that up, promotes it to Pending,
    //   and dispatches via `dispatch_to_plugin_with_judgement` with
    //   `fire_after_judging=false`.
    //
    // No handler-side `tokio::spawn` is needed in either branch - a
    // crash between txn.commit() and the response no longer loses the
    // rejudge.

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response =
        build_submission_response(&state.db, &*state.blob_store, updated, visibility).await?;
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let response = apply_filter_to_response_after_mutation(&kernel, response).await?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/bulk-rejudge",
    tag = "Submissions",
    operation_id = "bulkRejudgeSubmissions",
    summary = "Bulk rejudge submissions",
    description = "Re-queues submissions in the provided ID list for rejudging. Max 10,000 IDs per request. Requires `submission:rejudge` permission.",
    request_body = BulkRejudgeRequest,
    responses(
        (status = 200, description = "Submissions re-queued", body = BulkRejudgeResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn bulk_rejudge_submissions(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<BulkRejudgeRequest>,
) -> Result<Json<BulkRejudgeResponse>, AppError> {
    auth_user.require_permission(perm::SUBMISSION_REJUDGE)?;
    validate_bulk_rejudge(&payload)?;
    // UP#39 backpressure-on-post: bulk rejudge can insert hundreds of
    // `Queued` rows in one call. The check here samples the depth
    // **before** the bulk insert, which is intentionally permissive:
    // the cap is a steady-state circuit-breaker, not a strict per-row
    // gate. A bulk request that arrives just below the cap may take
    // the depth meaningfully past it for the rest of that call's
    // duration; the next caller will be rejected, restoring
    // equilibrium. Sampling per-row would serialize bulk-rejudge into
    // hundreds of count round-trips and defeat the bulk endpoint.
    enforce_queue_depth_admission(&state).await?;

    // `target_worker_id=Some("")` is a sentinel for "clear pin"; any explicit
    // routing change requires admin. `None` means leave existing pins alone.
    let new_target: Option<Option<String>> = match payload.target_worker_id.as_deref() {
        None => None,
        Some("") => {
            auth_user.require_permission(perm::SYSTEM_ADMIN)?;
            Some(None)
        }
        Some(raw) => {
            auth_user.require_permission(perm::SYSTEM_ADMIN)?;
            let live = crate::handlers::system::live_worker_ids(&state).await;
            if !live.contains(raw) {
                return Err(AppError::Validation(format!(
                    "Worker '{raw}' has no live heartbeat"
                )));
            }
            Some(Some(raw.to_string()))
        }
    };

    let requested = payload.submission_ids.len();
    let mut requested_ids = payload.submission_ids;
    requested_ids.sort_unstable();
    requested_ids.dedup();
    let requested_unique = requested_ids.len();

    let all_ids: Vec<i32> = submission::Entity::find()
        .filter(submission::Column::Id.is_in(requested_ids.clone()))
        .select_only()
        .column(submission::Column::Id)
        .order_by_asc(submission::Column::Id)
        .into_tuple()
        .all(&state.db)
        .await?;

    if all_ids.is_empty() {
        return Ok(Json(BulkRejudgeResponse { queued: 0 }));
    }

    const BATCH_SIZE: usize = 500;
    // Per-batch counter: rows the user successfully re-queued (immediate
    // path -> submission row in `Queued`; deferred path -> judgement row
    // in `Queued`). After UP#37 + the residual fix, both branches are
    // claim-fiber-driven and the handler never spawns.
    let mut queued: usize = 0;

    for batch_ids in all_ids.chunks(BATCH_SIZE) {
        let txn = state.db.begin().await?;

        let locked = submission::Entity::find()
            .filter(submission::Column::Id.is_in(batch_ids.to_vec()))
            .lock(LockType::Update)
            .all(&txn)
            .await?;

        for sub in &locked {
            let resolved_target = match new_target.clone() {
                Some(t) => t,
                None => sub.target_worker_id.clone(),
            };
            let new_epoch = sub.judge_epoch.saturating_add(1);
            let _new_judgement = open_rejudge_judgement(
                &txn,
                sub,
                auth_user.user_id,
                resolved_target.clone(),
                None,
                new_epoch,
                payload.apply_immediately,
            )
            .await?;

            if payload.apply_immediately {
                let mut active: submission::ActiveModel = sub.clone().into();
                // UP#37: durable accept - flip to `Queued`, claim fiber
                // (UP#38) will promote to `Pending` and dispatch. See
                // single-submission rejudge handler above for the full
                // narrative.
                active.status = Set(SubmissionStatus::Queued);
                active.clear_judgement_columns();
                active.judge_epoch = Set(new_epoch);
                if let Some(target) = new_target.clone() {
                    active.target_worker_id = Set(target);
                }
                active.update(&txn).await?;
            }
            // Deferred branch (`apply_immediately=false`): the
            // judgement row was inserted at status=Queued (see
            // `open_rejudge_judgement`); the claim fiber's judgement
            // scan will dispatch it via
            // `dispatch_to_plugin_with_judgement(..., fire_after_judging=false)`.
            // No handler-side spawn.
            queued += 1;
        }

        txn.commit().await?;
    }

    info!(
        user_id = auth_user.user_id,
        requested, requested_unique, queued, "Bulk rejudge completed"
    );

    Ok(Json(BulkRejudgeResponse { queued }))
}

#[utoipa::path(
    post,
    path = "/submissions/fan-out",
    tag = "Admin",
    operation_id = "adminFanOutSubmission",
    summary = "Fan out a submission to a set of workers",
    description = "Creates one submission per worker in `target_worker_ids`, each pinned to the named worker. Used by admins to verify that specific lab workers run a known-good (or known-bad) solution correctly. Skips the per-user submission rate limit. Requires `system:admin` permission.",
    request_body = AdminFanOutSubmissionRequest,
    responses(
        (status = 201, description = "Submissions created", body = AdminFanOutSubmissionResponse),
        (status = 400, description = "Validation error or worker offline (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem or contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn admin_fan_out_submission(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<AdminFanOutSubmissionRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::SYSTEM_ADMIN)?;
    validate_admin_fan_out(&payload)?;
    validate_code_payload(
        &payload.files,
        &payload.language,
        state.config.submission.max_size,
    )?;
    // UP#39 backpressure-on-post: admin fan-out inserts one `Queued`
    // row per target worker, so the same sampling-before-bulk-insert
    // tradeoff applies as `bulk_rejudge_submissions`. A successful
    // fan-out may temporarily exceed the cap; subsequent callers
    // observe the bumped depth and back off.
    enforce_queue_depth_admission(&state).await?;

    let live_workers = crate::handlers::system::live_worker_ids(&state).await;
    let mut offline: Vec<&str> = payload
        .target_worker_ids
        .iter()
        .filter(|id| !live_workers.contains(*id))
        .map(String::as_str)
        .collect();
    offline.sort();
    if !offline.is_empty() {
        return Err(AppError::Validation(format!(
            "target_worker_ids contains worker(s) without a live heartbeat: {}",
            offline.join(", ")
        )));
    }

    let txn = state.db.begin().await?;

    let problem = find_problem(&txn, payload.problem_id).await?;
    let known_languages: std::collections::HashSet<String> = state
        .registries
        .language_resolver_registry
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    validate_submission_contract(
        &payload.files,
        &payload.language,
        problem.get_submission_format(),
        &known_languages,
    )?;

    if let Some(contest_id) = payload.contest_id {
        let _contest = find_contest(&txn, contest_id).await?;
        if !is_problem_in_contest(&txn, contest_id, payload.problem_id).await? {
            return Err(AppError::NotFound(
                "Problem not found in this contest".into(),
            ));
        }
    }

    let contest_type = match payload.contest_type {
        Some(ref ct) => {
            let registry = state.registries.contest_type_registry.read().await;
            if !registry.contains_key(ct) {
                let mut valid: Vec<_> = registry.keys().cloned().collect();
                valid.sort();
                return Err(AppError::Validation(format!(
                    "contest_type must be one of: {}",
                    valid.join(", ")
                )));
            }
            ct.clone()
        }
        None => match payload.contest_id {
            Some(contest_id) => {
                let contest_model = find_contest(&txn, contest_id).await?;
                contest_model
                    .contest_type
                    .clone()
                    .unwrap_or_else(|| problem.default_contest_type.clone())
            }
            None => problem.default_contest_type.clone(),
        },
    };

    let language = payload.language.trim().to_string();
    let now = Utc::now();
    let files_json = files_to_json(&payload.files);

    let mut models = Vec::with_capacity(payload.target_worker_ids.len());
    for worker_id in &payload.target_worker_ids {
        let new_submission = submission::ActiveModel {
            files: Set(files_json.clone()),
            language: Set(language.clone()),
            // UP#37: admin fan-out submissions enter the same
            // durable-accept lifecycle. The claim fiber (UP#38)
            // promotes each row from `Queued` to `Pending` and
            // dispatches via the worker-pinned route already encoded
            // in `target_worker_id`.
            status: Set(SubmissionStatus::Queued),
            user_id: Set(auth_user.user_id),
            problem_id: Set(payload.problem_id),
            contest_id: Set(payload.contest_id),
            contest_type: Set(contest_type.clone()),
            target_worker_id: Set(Some(worker_id.clone())),
            created_at: Set(now),
            ..Default::default()
        };
        let model = new_submission.insert(&txn).await?;
        models.push(model);
    }

    txn.commit().await?;

    info!(
        admin_user_id = auth_user.user_id,
        problem_id = payload.problem_id,
        contest_id = ?payload.contest_id,
        worker_count = models.len(),
        "Admin fan-out submission created"
    );

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    let mut responses = Vec::with_capacity(models.len());
    for model in models {
        let response =
            build_submission_response(&state.db, &*state.blob_store, model, visibility).await?;
        let response = apply_filter_to_response_after_mutation(&kernel, response).await?;
        responses.push(response);
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "submissions": responses })),
    ))
}
