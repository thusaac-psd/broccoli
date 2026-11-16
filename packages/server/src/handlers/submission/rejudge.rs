use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
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
use crate::entity::submission;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::FreshAuthUser;
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::problem::validate_contest_type;
use crate::models::submission::*;
use crate::services::judging::{
    self, ApplyJudgement, DiscardJudgement, OpenJudgement, apply_judgement, discard_judgement,
    open_judgement,
};
use crate::services::submission_dispatch::fire_after_judging_hooks;
use crate::state::AppState;
use crate::utils::contest::{find_contest, is_problem_in_contest, required_contest_type};
use crate::utils::judging::{files_to_json, validate_code_payload, validate_submission_contract};
use crate::utils::problem::find_problem;
use crate::visibility::{Subject, VisibilityKernel};

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
        (status = 200, description = "Applied judgement", body = SubmissionResponseAfterMutation),
        (status = 400, description = "Judgement is not finalized (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission or judgement not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
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
    let updated = match apply_judgement(&txn, id, judgement_id).await? {
        ApplyJudgement::Applied(updated) => *updated,
        ApplyJudgement::SubmissionNotFound => {
            return Err(AppError::NotFound("Submission not found".into()));
        }
        ApplyJudgement::JudgementNotFound => {
            return Err(AppError::NotFound("Judgement not found".into()));
        }
        ApplyJudgement::NotFinalized => {
            return Err(AppError::Validation(
                "Cannot apply a judgement that is not finalized".into(),
            ));
        }
    };
    txn.commit().await?;

    fire_after_judging_hooks(
        &state.db,
        state.registries.hook_registry.clone(),
        &state.registries.contest_type_registry,
        updated.id,
        updated.user_id,
        updated.problem_id,
        updated.contest_id,
    )
    .await;

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response = build_submission_response(
        &state.db,
        &*state.blob_store,
        state.clock.now(),
        updated,
        visibility,
    )
    .await?;
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
    description = "Deletes a non-current judgement and its test-case result rows, and resolves its dead letter message, if any. Requires `submission:rejudge` permission.",
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
    match discard_judgement(&txn, id, judgement_id, auth_user.user_id).await? {
        DiscardJudgement::Discarded => {}
        DiscardJudgement::SubmissionNotFound => {
            return Err(AppError::NotFound("Submission not found".into()));
        }
        DiscardJudgement::JudgementNotFound => {
            return Err(AppError::NotFound("Judgement not found".into()));
        }
        DiscardJudgement::IsCurrent => {
            return Err(AppError::Conflict(
                "Cannot discard the current judgement".into(),
            ));
        }
        DiscardJudgement::NotFinalized => {
            return Err(AppError::Conflict(
                "Cannot discard a judgement that is still running".into(),
            ));
        }
    }
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
        (status = 200, description = "Submission re-queued", body = SubmissionResponseAfterMutation),
        (status = 400, description = "Invalid worker (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED); or a plugin it needs could not answer in time (PLUGIN_UNAVAILABLE)", body = ErrorBody),
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
    // Backpressure on POST: both branches queue a judgement, which
    // `count_queued_rows()` counts, so the cap applies uniformly.
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

    // `open_judgement` allocates the new judgement's epoch, resets the
    // submission (apply-immediately) and inserts the judgement, all in this
    // transaction. Both branches are durable: the claim fiber picks up a
    // `Queued` submission (apply-immediately) or a `Queued` judgement
    // (deferred, dispatched with `fire_after_judging=false`), so no
    // handler-side spawn is needed and a crash after commit cannot lose the
    // rejudge. The prior verdict and its test_case_result rows stay attached
    // to the old judgement as version history.
    let txn = state.db.begin().await?;
    let open = if payload.apply_immediately {
        OpenJudgement::Rejudge {
            triggered_by_user_id: auth_user.user_id,
            target_worker_id: new_target,
        }
    } else {
        OpenJudgement::DeferredRejudge {
            triggered_by_user_id: auth_user.user_id,
            target_worker_id: new_target,
        }
    };
    let (updated, _judgement) = open_judgement(&txn, id, open)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))?;
    txn.commit().await?;

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response = build_submission_response(
        &state.db,
        &*state.blob_store,
        state.clock.now(),
        updated,
        visibility,
    )
    .await?;
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let response = apply_filter_to_response_after_mutation(&kernel, response).await?;
    Ok(Json(response))
}

/// Bulk-rejudges every submission in `payload.submission_ids` that exists,
/// returning only `{ queued: N }` - never a per-id success/failure list,
/// never any submission content.
///
/// `N` counts every *requested, deduplicated* id found in the `submission`
/// table, regardless of the caller's own `Resource::Submission` Read
/// visibility into any of them - this endpoint is gated on
/// `submission:rejudge`, a mutation permission independent of Read
/// reachability (see `apply_filter_to_response_after_mutation`'s doc comment
/// in `filter.rs` for the sibling reasoning on the single-submission path).
/// A caller who repeatedly narrows `submission_ids` and reads back `queued`
/// can therefore binary-search which ids exist, even ones they could never
/// `GET` - an aggregate *existence* oracle over the id space.
///
/// That is a real disclosure; do not describe it as "nothing to leak". It is
/// judged acceptable because it discloses existence only, never content: no
/// field of a submission the caller can't Read (owner, verdict, code,
/// timestamps, ...) is ever observable through this count, only whether the
/// row is present. That is the same disclosure class the single-submission
/// surface has - `GET /submissions/{id}` answers 200 vs. 404 for exactly this
/// reason, and `apply_submission_judgement`/`rejudge_submission`
/// succeed-or-404 on a nonexistent id - just aggregated across up to
/// [`MAX_BULK_REJUDGE_IDS`] ids in one round trip instead of one id per
/// request. Don't close it by changing this
/// endpoint's behaviour alone: the identical oracle stays reachable one id
/// at a time via the endpoints above, so narrowing only this one would trade
/// a fast leak for a slow one rather than closing it.
#[utoipa::path(
    post,
    path = "/bulk-rejudge",
    tag = "Submissions",
    operation_id = "bulkRejudgeSubmissions",
    summary = "Bulk rejudge submissions",
    description = format!("Re-queues submissions in the provided ID list for rejudging. Max {MAX_BULK_REJUDGE_IDS} IDs per request. Requires `submission:rejudge` permission."),
    request_body = BulkRejudgeRequest,
    responses(
        (status = 200, description = "Submissions re-queued", body = BulkRejudgeResponse),
        (status = 400, description = "Empty or oversized id list, non-positive id, or invalid worker (VALIDATION_ERROR)", body = ErrorBody),
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
    // Backpressure on POST: bulk rejudge can insert hundreds of
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
    // Rows the caller re-queued: a `Queued` submission (apply-immediately)
    // or a `Queued` deferred judgement, both picked up by the claim fiber,
    // so the handler never spawns. Ids are visited in ascending order, so
    // two concurrent bulk rejudges take the submission row locks in the same
    // order. A row deleted since the existence scan above is skipped.
    let mut queued: usize = 0;

    for batch_ids in all_ids.chunks(BATCH_SIZE) {
        let txn = state.db.begin().await?;
        for &id in batch_ids {
            let open = if payload.apply_immediately {
                OpenJudgement::Rejudge {
                    triggered_by_user_id: auth_user.user_id,
                    target_worker_id: new_target.clone(),
                }
            } else {
                OpenJudgement::DeferredRejudge {
                    triggered_by_user_id: auth_user.user_id,
                    target_worker_id: new_target.clone(),
                }
            };
            if open_judgement(&txn, id, open).await?.is_some() {
                queued += 1;
            }
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
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED); or a plugin it needs could not answer in time (PLUGIN_UNAVAILABLE)", body = ErrorBody),
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
    // Backpressure on POST: admin fan-out inserts one `Queued`
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

    // A contest submission is judged under its contest's type, like every
    // other submission of that contest; only a practice submission may name
    // another.
    let contest_type = match payload.contest_id {
        Some(contest_id) => {
            let contest_type = required_contest_type(&find_contest(&txn, contest_id).await?)?;
            if !is_problem_in_contest(&txn, contest_id, payload.problem_id).await? {
                return Err(AppError::NotFound(
                    "Problem not found in this contest".into(),
                ));
            }
            if payload
                .contest_type
                .as_ref()
                .is_some_and(|ct| *ct != contest_type)
            {
                return Err(AppError::Validation(format!(
                    "A submission to this contest is judged as '{contest_type}', its contest type"
                )));
            }
            contest_type
        }
        None => match payload.contest_type {
            Some(ref ct) => {
                validate_contest_type("contest_type", ct, &state.registries.contest_type_registry)
                    .await?;
                ct.clone()
            }
            None => problem.default_contest_type.clone(),
        },
    };

    let language = payload.language.trim().to_string();
    let files_json = files_to_json(&payload.files);

    let mut models = Vec::with_capacity(payload.target_worker_ids.len());
    for worker_id in &payload.target_worker_ids {
        let new_submission = submission::ActiveModel {
            files: Set(files_json.clone()),
            language: Set(language.clone()),
            user_id: Set(auth_user.user_id),
            problem_id: Set(payload.problem_id),
            contest_id: Set(payload.contest_id),
            contest_type: Set(contest_type.clone()),
            target_worker_id: Set(Some(worker_id.clone())),
            ..Default::default()
        };
        // Queued with its first judgement, pinned to the worker through
        // `target_worker_id`; the claim fiber dispatches it.
        let (model, _judgement) = judging::create_submission(&txn, new_submission).await?;
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
        let response = build_submission_response(
            &state.db,
            &*state.blob_store,
            state.clock.now(),
            model,
            visibility,
        )
        .await?;
        let response = apply_filter_to_response_after_mutation(&kernel, response).await?;
        responses.push(response);
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "submissions": responses })),
    ))
}
