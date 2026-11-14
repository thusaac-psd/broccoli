use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::types::BeforeSubmissionEvent;
use sea_orm::*;
use tracing::instrument;

use crate::dispatcher::queue_depth::enforce_queue_depth_admission;
// visibility-bypass-audited: every read handler in this module already
// routes through `VisibilityKernel` (`get_submission`/`list_submission_judgements`
// via `Resource::Submission`, `list_submissions`/`list_contest_submissions`
// via `fetch_visible_batch`, `create_submission`/`create_contest_submission`
// via `Resource::Problem` before accepting a submit) - these entity types are
// only used for the row fetch that feeds the kernel. `list_contest_submissions`'s
// top gate also routes through the kernel: it decides `Action::Read` on
// `Resource::Contest(contest_id)` (the same gate `get_contest`/
// `list_contest_problems` use, applying the window/is_public/participant
// rule in `decide_contest`), reusing the single per-request kernel instance that also drives
// the per-row `apply_filter_to_list` pass on `Resource::Submission` further
// down - distinct resource kinds, so the two decisions cannot collide or
// double-decide (see the comments at that call site), pinned by the frozen
// `tests/integration/visibility_matrix.rs::contest_submission_list` and
// `submission_detail` suites.
use crate::entity::{contest, contest_user, problem, submission, submission_judgement, user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::hooks;
use crate::models::problem::validate_contest_type;
use crate::models::shared::{PageWindow, escape_like};
use crate::models::submission::*;
use crate::services::judging;
use crate::state::AppState;
use crate::utils::contest::{
    compile_output_public, find_contest, require_contest_participant, require_contest_running,
    required_contest_type,
};
use crate::utils::judging::{files_to_json, validate_code_payload, validate_submission_contract};
use crate::utils::problem::find_problem;
use crate::utils::query::validate_sorting_params;
use crate::utils::rate_limit::check_rate_limit;
use crate::visibility::{Action, Resource, ResourceScope, Subject, VisibilityKernel};

mod dispatch;
mod filter;
mod list_page;
mod rejudge;
mod response;

pub use rejudge::*;

// Re-exported for the DLQ retry path, which is an immediate rejudge and must
// prepare the judgement lineage through the same primitive as the admin
// rejudge endpoints (see `crate::handlers::dlq`).

use dispatch::{dispatch_before_submission_hooks, find_submission, fire_after_submission_hooks};
use filter::{apply_filter_to_judgement_response, apply_filter_to_response};
use response::{
    VisibilityContext, build_judgement_response, build_submission_response,
    build_submission_response_with_cases,
};

#[utoipa::path(
    post,
    path = "/",
    tag = "Submissions",
    operation_id = "createSubmission",
    summary = "Submit a solution to a problem",
    description = "Creates a new submission for the specified problem. The submission will be queued for judging. Requires `submission:submit` permission.",
    params(
        ("id" = i32, Path, description = "Problem ID")
    ),
    request_body = CreateSubmissionRequest,
    responses(
        (status = 201, description = "Submission created", body = SubmissionResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 429, description = "Per-user rate limit or plugin rejection (RATE_LIMITED, PLUGIN_REJECTED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED); or a plugin it needs could not answer in time (PLUGIN_UNAVAILABLE)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(problem_id = %problem_id))]
pub async fn create_submission(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    AppJson(payload): AppJson<CreateSubmissionRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::SUBMISSION_SUBMIT)?;
    validate_code_payload(
        &payload.files,
        &payload.language,
        state.config.submission.max_size,
    )?;
    check_rate_limit(
        &state.db,
        state.clock.now(),
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // Backpressure on POST: shed load before touching the DB so
    // the rejected request never holds a connection or a row lock.
    enforce_queue_depth_admission(&state).await?;

    // No wrapping transaction. These are independent read validations followed
    // by a single-row INSERT (atomic on its own), so a txn buys no real
    // consistency here (READ COMMITTED, no row locks). Holding a pooled txn
    // connection open across the later `&state.db` acquisition
    // (`fetch_resource_enablements`) and the slow WASM `before_submission`
    // dispatch caused a core-pool self-deadlock once concurrent submits reached
    // the pool size: every request parked `idle in transaction` while waiting to
    // check out a second connection that never freed. Run each step on the pool
    // directly so a request never holds two connections at once.
    let problem = find_problem(&state.db, problem_id).await?;
    // The kernel OWNS the subject: one kernel per request per subject.
    // REACHABILITY FIRST, before the `before_submission` hook dispatch below.
    // Without this a contestant can probe and submit against hidden or
    // unreleased problems by guessing IDs. That is a stronger information
    // oracle than viewing, because a submission runs the secret tests. A
    // denied decision fails closed and silent (404). This is reachability,
    // not a business rule, so it must never grow a reason string. The
    // cooldown/submission-limit plugins' own loud, reasoned rejections are a
    // separate, later check that this gate does not touch.
    //
    // `Action::Submit` is stricter than viewing. Only a public problem or a
    // problem editor gets through here. A hidden problem is denied even when
    // one of its contests would let the caller read it, because this row is
    // stored with `contest_id = NULL` and would skip that contest's plugin
    // config, feedback masking and standings. Contestants submit through
    // `create_contest_submission` instead. See
    // `visibility::host_rules::standalone_reachable_via_contest`.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    if kernel
        .decide(
            Action::Submit,
            Resource::Problem {
                contest_id: None,
                problem_id,
            },
        )
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Problem not found".into()));
    }
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

    let contest_type = match payload.contest_type {
        Some(ref ct) => {
            validate_contest_type("contest_type", ct, &state.registries.contest_type_registry)
                .await?;
            ct.clone()
        }
        None => problem.default_contest_type.clone(),
    };

    let attempt_id = uuid::Uuid::new_v4();
    let hook_event = BeforeSubmissionEvent {
        attempt_id: attempt_id.to_string(),
        attempt_timeout_ms: dispatch::ATTEMPT_TIMEOUT.as_millis() as u64,
        user_id: auth_user.user_id,
        problem_id,
        contest_id: None,
        language: payload.language.trim().to_string(),
        file_count: payload.files.len(),
        contest_type: contest_type.clone(),
    };
    let enabled_plugins = hooks::resource_enablements(
        &state.db,
        &state.registries.contest_type_registry,
        problem_id,
        None,
    )
    .await?;
    dispatch_before_submission_hooks(&state, &hook_event, Some(&enabled_plugins), None).await?;

    let language = payload.language.trim().to_string();
    let new_submission = submission::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language.clone()),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set(contest_type),
        attempt_id: Set(Some(attempt_id)),
        ..Default::default()
    };

    // Persisted `Queued` with its first judgement; the claim fiber dispatches
    // it. Dispatching from the handler instead would lose the submission if
    // the api crashed between commit and dispatch.
    let txn = state.db.begin().await?;
    let (model, _judgement) = judging::create_submission(&txn, new_submission).await?;
    txn.commit().await?;

    fire_after_submission_hooks(
        &state,
        model.id,
        auth_user.user_id,
        problem_id,
        None,
        language,
        Some(enabled_plugins),
    );

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response = build_submission_response(
        &state.db,
        &*state.blob_store,
        state.clock.now(),
        model,
        visibility,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Submissions",
    operation_id = "listSubmissions",
    summary = "List submissions",
    description = "Returns one cursor page of submissions. Users see their own submissions; users with `submission:view_all` permission see all submissions. A page holds only submissions the caller is shown; pass `pagination.next_cursor` as `cursor` for the next one. `pagination.total` and `pagination.total_pages` are null when a visibility plugin may hide matching submissions from the caller, since a count would disclose them.",
    params(SubmissionListQuery),
    responses(
        (status = 200, description = "List of submissions", body = SubmissionListResponse),
        (status = 400, description = "Invalid query or cursor (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn list_submissions(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<SubmissionListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_sorting_params(
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        &["created_at", "status"],
    )?;

    let can_view_all = auth_user.has_permission(perm::SUBMISSION_VIEW_ALL);

    let per_page = PageWindow::page_size(query.per_page);

    let mut base_select = submission::Entity::find();

    if !can_view_all {
        base_select = base_select.filter(submission::Column::UserId.eq(auth_user.user_id));
    }

    if let Some(pid) = query.problem_id {
        base_select = base_select.filter(submission::Column::ProblemId.eq(pid));
    }
    if let Some(uid) = query.user_id
        && (can_view_all || uid == auth_user.user_id)
    {
        base_select = base_select.filter(submission::Column::UserId.eq(uid));
    }
    if let Some(ref lang) = query.language {
        base_select = base_select.filter(submission::Column::Language.eq(lang.trim()));
    }
    if let Some(status) = query.status {
        base_select = base_select.filter(
            crate::services::submission_feedback::public_status(can_view_all)
                .eq(status.to_string()),
        );
    }
    if let Some(ref raw) = query.q {
        let escaped = escape_like(raw.trim());
        if !escaped.is_empty() {
            use sea_orm::prelude::Expr;
            use sea_orm::sea_query::{Func, LikeExpr, Query as SeaQuery};

            let pattern = format!("%{}%", escaped.to_lowercase());
            let user_subq = SeaQuery::select()
                .column(user::Column::Id)
                .from(user::Entity)
                .and_where(
                    Expr::expr(Func::lower(Expr::col(user::Column::Username)))
                        .like(LikeExpr::new(&pattern).escape('\\')),
                )
                .to_owned();
            let problem_subq = SeaQuery::select()
                .column(problem::Column::Id)
                .from(problem::Entity)
                .and_where(
                    Expr::expr(Func::lower(Expr::col(problem::Column::Title)))
                        .like(LikeExpr::new(&pattern).escape('\\')),
                )
                .to_owned();
            let contest_subq = SeaQuery::select()
                .column(contest::Column::Id)
                .from(contest::Entity)
                .and_where(
                    Expr::expr(Func::lower(Expr::col(contest::Column::Title)))
                        .like(LikeExpr::new(&pattern).escape('\\')),
                )
                .to_owned();

            base_select = base_select.filter(
                Condition::any()
                    .add(submission::Column::UserId.in_subquery(user_subq))
                    .add(submission::Column::ProblemId.in_subquery(problem_subq))
                    .add(submission::Column::ContestId.in_subquery(contest_subq)),
            );
        }
    }

    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    // Every row `base_select` matches is host-`Allow` for this viewer (a
    // non-`submission:view_all` caller is restricted to their own rows
    // above), so only a visibility plugin can hide a matching row. A count
    // is exact, and reported, only when none can.
    // The list spans contests of any type.
    let total = list_total(&state, &kernel, &base_select, &ResourceScope::Unknown).await?;

    // This list is GLOBAL - it can span several contests plus contest-less
    // submissions in one page. Each row is decided as its own
    // `Resource::Submission(id)`, whose contest scope the kernel resolves
    // internally, so rows from different contests never share a decision.
    let order = list_page::ListOrder::from_query(
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        can_view_all,
    );
    let (data, pagination) = list_page::visible_page(
        &state,
        &kernel,
        base_select,
        &list_page::filtered_fields(&query),
        order,
        query.cursor.as_deref(),
        per_page,
        total,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "data": data,
        "pagination": pagination,
    })))
}

/// The exact number of rows `select` matches, or `None` when a visibility
/// plugin may hide some of them from this viewer (see
/// [`VisibilityKernel::plugins_may_narrow`]). `select` must already encode
/// the host visibility rules for the viewer, and every row it matches must
/// fall under `scope`.
async fn list_total(
    state: &AppState,
    kernel: &VisibilityKernel<'_>,
    select: &Select<submission::Entity>,
    scope: &ResourceScope,
) -> Result<Option<u64>, AppError> {
    if kernel.plugins_may_narrow(scope) {
        return Ok(None);
    }
    Ok(Some(select.clone().count(&state.db).await?))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Submissions",
    operation_id = "getSubmission",
    summary = "Get submission details",
    description = "Returns full details of a submission. Users can view their own submissions; users with `submission:view_all` permission can view any submission.",
    params(
        ("id" = i32, Path, description = "Submission ID"),
        SubmissionResultQuery,
    ),
    responses(
        (status = 400, description = "Invalid testcase page", body = ErrorBody),
        (status = 200, description = "Submission details", body = SubmissionResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query), fields(submission_id = %id))]
pub async fn get_submission(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    Query(query): Query<SubmissionResultQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let resource = Resource::Submission(id);

    // The kernel decides on the row read here instead of reading it again.
    // A missing and an unreachable submission answer the same 404.
    let sub = find_submission(&state.db, id).await?;
    kernel.know_submission(sub.clone());
    // Fail fast, before building the full detail response, on a submission
    // the kernel finds unreachable: the owner bypass, the contest activation
    // window, participation and `submissions_visible` (see
    // `visibility::host_rules::decide_submission`).
    if kernel.decide(Action::Read, resource).await?.is_denied() {
        return Err(AppError::NotFound("Submission not found".into()));
    }

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response = build_submission_response_with_cases(
        &state.db,
        &*state.blob_store,
        state.clock.now(),
        sub,
        visibility,
        &query,
    )
    .await?;

    // The DTO reaches the response body only through `into_masked_json`
    // (inside `apply_filter_to_response`), so a `Redact` decision - host or
    // plugin - can never be forgotten at the serialization step. The kernel
    // memoizes per `(Action, Resource)`, so this re-decides the same
    // `resource` already checked above at no extra DB/plugin cost.
    let response = apply_filter_to_response(&kernel, response).await?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/{id}/judgements",
    tag = "Submissions",
    operation_id = "listSubmissionJudgements",
    summary = "List submission judgement versions",
    description = "Returns all judgement versions for a submission. Visibility matches `getSubmission`.",
    params(
        ("id" = i32, Path, description = "Submission ID")
    ),
    responses(
        (status = 200, description = "Submission judgement versions", body = Vec<SubmissionJudgementResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Submission not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(submission_id = %id))]
pub async fn list_submission_judgements(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let resource = Resource::Submission(id);

    // Fail fast, exactly as `get_submission`: a submission the kernel
    // already knows is unreachable never gets its judgement history built.
    if kernel.decide(Action::Read, resource).await?.is_denied() {
        return Err(AppError::NotFound("Submission not found".into()));
    }

    let sub = find_submission(&state.db, id).await?;
    let visibility = VisibilityContext::from_auth_user(&auth_user);

    let judgements = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
        .order_by_asc(submission_judgement::Column::Version)
        .all(&state.db)
        .await?;

    // The full version history exposes in-progress / pending admin regrades and
    // superseded verdicts. Only viewers who can rejudge (or see all submissions)
    // may see the history; everyone else, including the submission owner, sees
    // only the current published judgement. Gating this only in the web client
    // would still leak the history to a direct API call.
    let can_see_history =
        visibility.has_view_all || auth_user.has_permission(perm::SUBMISSION_REJUDGE);
    let judgements: Vec<_> = if can_see_history {
        judgements
    } else {
        judgements.into_iter().filter(|j| j.is_current).collect()
    };

    let mut responses = Vec::with_capacity(judgements.len());
    for judgement in judgements {
        let response =
            build_visible_judgement(&state, &kernel, &sub, &visibility, judgement, None).await?;
        responses.push(response);
    }

    Ok(Json(responses))
}

async fn build_visible_judgement(
    state: &AppState,
    kernel: &VisibilityKernel<'_>,
    sub: &submission::Model,
    visibility: &VisibilityContext,
    judgement: submission_judgement::Model,
    query: Option<&SubmissionResultQuery>,
) -> Result<serde_json::Value, AppError> {
    let problem_model = problem::Entity::find_by_id(sub.problem_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::Internal("Submission problem not found".into()))?;
    let user_model = user::Entity::find_by_id(sub.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::Internal("Submission user not found".into()))?;
    let contest_model = if let Some(contest_id) = sub.contest_id {
        Some(
            contest::Entity::find_by_id(contest_id)
                .one(&state.db)
                .await?
                .ok_or_else(|| AppError::Internal("Contest not found".into()))?,
        )
    } else {
        None
    };

    let is_owner = visibility.viewer_id == sub.user_id;
    let show_compile_output = visibility.has_view_all
        || is_owner
        || compile_output_public(contest_model.as_ref(), state.clock.now());
    let show_test_details = visibility.has_view_all || problem_model.show_test_details;

    let mut response = build_judgement_response(
        &state.db,
        &*state.blob_store,
        judgement,
        show_compile_output,
        show_test_details,
        query,
    )
    .await?;
    if visibility.has_view_all && !response.is_current {
        let row = state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "WITH old_results AS (
                 SELECT r.* FROM test_case_result r
                 JOIN submission_judgement j
                   ON j.id = r.judgement_id AND j.judge_epoch = r.judge_epoch
                 WHERE r.judgement_id = $2
             ), current_results AS (
                 SELECT r.* FROM test_case_result r
                 JOIN submission_judgement j
                   ON j.id = r.judgement_id AND j.judge_epoch = r.judge_epoch
                 WHERE j.submission_id = $1 AND j.is_current
             )
             SELECT COUNT(*) AS count
             FROM old_results r
             FULL OUTER JOIN current_results c
             ON c.test_case_id = r.test_case_id
             WHERE r.id IS NULL OR c.id IS NULL OR
               (r.verdict, r.score, r.time_used, r.memory_used, r.checker_output)
               IS DISTINCT FROM
               (c.verdict, c.score, c.time_used, c.memory_used, c.checker_output)",
                [sub.id.into(), response.id.into()],
            ))
            .await?;
        response.case_changes = row
            .map(|r| r.try_get::<i64>("", "count"))
            .transpose()?
            .map(|n| n as u64);
    }
    apply_filter_to_judgement_response(
        kernel,
        sub,
        &user_model,
        &problem_model,
        response,
        visibility,
    )
    .await
}
#[utoipa::path(
    get,
    path = "/{id}/judgements/{judgement_id}",
    tag = "Submissions",
    operation_id = "getSubmissionJudgement",
    summary = "Get one judgement with a bounded testcase page",
    params(
        ("id" = i32, Path, description = "Submission ID"),
        ("judgement_id" = i32, Path, description = "Judgement ID"),
        SubmissionResultQuery,
    ),
    responses(
        (status = 200, description = "Judgement details", body = SubmissionJudgementResponse),
        (status = 400, description = "Invalid result page", body = ErrorBody),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 404, description = "Judgement not found", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn get_submission_judgement(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((id, judgement_id)): AppPath<(i32, i32)>,
    Query(query): Query<SubmissionResultQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    if kernel
        .decide(Action::Read, Resource::Submission(id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Submission not found".into()));
    }
    let sub = find_submission(&state.db, id).await?;
    let visibility = VisibilityContext::from_auth_user(&auth_user);
    let judgement = submission_judgement::Entity::find_by_id(judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(id))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Judgement not found".into()))?;
    if !judgement.is_current
        && !visibility.has_view_all
        && !auth_user.has_permission(perm::SUBMISSION_REJUDGE)
    {
        return Err(AppError::NotFound("Judgement not found".into()));
    }
    Ok(Json(
        build_visible_judgement(&state, &kernel, &sub, &visibility, judgement, Some(&query))
            .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Submissions",
    operation_id = "createContestSubmission",
    summary = "Submit a solution to a contest problem",
    description = "Creates a new submission for a problem within a contest. The user must be a contest participant (or have `contest:manage` permission), and the contest must be active. Requires `submission:submit` permission.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID")
    ),
    request_body = CreateSubmissionRequest,
    responses(
        (status = 201, description = "Submission created", body = SubmissionResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 429, description = "Per-user rate limit or plugin rejection (RATE_LIMITED, PLUGIN_REJECTED)", body = ErrorBody),
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED), or this server cannot judge the contest's type right now (CONTEST_TYPE_UNAVAILABLE); both carry Retry-After; or a plugin it needs could not answer in time (PLUGIN_UNAVAILABLE)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(id = %id, problem_id = %problem_id))]
pub async fn create_contest_submission(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((id, problem_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<CreateSubmissionRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::SUBMISSION_SUBMIT)?;
    validate_code_payload(
        &payload.files,
        &payload.language,
        state.config.submission.max_size,
    )?;
    check_rate_limit(
        &state.db,
        state.clock.now(),
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // Backpressure on POST: same rationale as `create_submission`.
    enforce_queue_depth_admission(&state).await?;

    let contest_id = id;
    // No wrapping transaction: see `create_submission` for the full rationale.
    // Holding a pooled txn connection open across the subsequent `&state.db`
    // acquisitions (`require_contest_participant`, `fetch_resource_enablements`)
    // and the slow WASM `before_submission` dispatch deadlocked the core pool
    // under sustained contest load (all connections parked `idle in
    // transaction`, each request then blocking to check out a second connection
    // that never freed). Every step runs on the pool directly so no request
    // holds two connections at once.
    let contest_model = find_contest(&state.db, contest_id).await?;

    let problem = find_problem(&state.db, problem_id).await?;
    // The kernel OWNS the subject: one kernel per request per subject.
    // REACHABILITY FIRST, BUSINESS RULES SECOND: this decision - contest
    // window/access plus contest-problem membership, ported verbatim from
    // `is_problem_in_contest` (see `visibility::host_rules::decide_problem_or_sample`)
    // - must run, and does, before `require_contest_running`/
    // `require_contest_participant` below and before the `before_submission`
    // hook dispatch further down. A denied decision fails closed and silent
    // (404); it must never grow a reason string. `require_contest_running`'s
    // "not started yet" / "already ended" and `require_contest_participant`'s
    // "forbidden" rejections are separate, later, loud business-rule checks -
    // this gate does not merge into them, and the cooldown/submission-limit
    // plugins' own rejections are untouched by it either.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    if kernel
        .decide(
            Action::Submit,
            Resource::Problem {
                contest_id: Some(contest_id),
                problem_id,
            },
        )
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound(
            "Problem not found in this contest".into(),
        ));
    }

    let now = state.clock.now();
    require_contest_running(&auth_user, &contest_model, now)?;
    require_contest_participant(&state.db, &auth_user, &contest_model).await?;
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

    let enabled_plugins = hooks::resource_enablements(
        &state.db,
        &state.registries.contest_type_registry,
        problem_id,
        Some(contest_id),
    )
    .await?;
    let contest_type = required_contest_type(&contest_model)?;
    // Pin the owner used to authorize hook effects through admission commit.
    let handler = state
        .registries
        .contest_type_registry
        .read()
        .await
        .get(&contest_type)
        .cloned();
    // Contest types are plugin-provided, and the handler owns admission: a
    // replica without one (its plugin still loading or failed here) must not
    // accept the submission unchecked. Reject before any hook runs.
    let Some(handler) = handler else {
        return Err(AppError::ContestTypeUnavailable(contest_type));
    };
    let admission_owner = handler
        .submission_admission
        .then_some(handler.plugin_id.as_str());
    let attempt_id = uuid::Uuid::new_v4();
    let hook_event = BeforeSubmissionEvent {
        attempt_id: attempt_id.to_string(),
        attempt_timeout_ms: dispatch::ATTEMPT_TIMEOUT.as_millis() as u64,
        user_id: auth_user.user_id,
        problem_id,
        contest_id: Some(contest_id),
        language: payload.language.trim().to_string(),
        file_count: payload.files.len(),
        contest_type: contest_type.clone(),
    };
    let admission = dispatch_before_submission_hooks(
        &state,
        &hook_event,
        Some(&enabled_plugins),
        admission_owner,
    )
    .await?;

    let language = payload.language.trim().to_string();
    let new_submission = submission::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language.clone()),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set(contest_type),
        attempt_id: Set(Some(attempt_id)),
        ..Default::default()
    };

    let model = match admission {
        Some(admission) if handler.submission_admission => {
            crate::services::submission_feedback::insert_admitted(
                &state,
                new_submission,
                &admission,
                &handler.plugin_id,
            )
            .await?
        }
        None if handler.submission_admission => {
            return Err(AppError::Internal(
                "Contest plugin did not provide submission admission".into(),
            ));
        }
        Some(_) => return Err(AppError::Internal("Unexpected submission admission".into())),
        None => {
            let txn = state.db.begin().await?;
            let (model, _judgement) = judging::create_submission(&txn, new_submission).await?;
            txn.commit().await?;
            model
        }
    };

    fire_after_submission_hooks(
        &state,
        model.id,
        auth_user.user_id,
        problem_id,
        Some(contest_id),
        language,
        Some(enabled_plugins),
    );

    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response = build_submission_response(
        &state.db,
        &*state.blob_store,
        state.clock.now(),
        model,
        visibility,
    )
    .await?;

    let response = apply_filter_to_response(&kernel, response).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Submissions",
    operation_id = "listContestSubmissions",
    summary = "List contest submissions",
    description = "Returns one cursor page of a contest's submissions. A page holds only submissions the caller is shown; pass `pagination.next_cursor` as `cursor` for the next one. `pagination.total` and `pagination.total_pages` are null when a visibility plugin may hide matching submissions from the caller, since a count would disclose them.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        SubmissionListQuery
    ),
    responses(
        (status = 200, description = "List of submissions", body = SubmissionListResponse),
        (status = 400, description = "Invalid query or cursor (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query), fields(contest_id = %contest_id))]
pub async fn list_contest_submissions(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    Query(query): Query<SubmissionListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_sorting_params(
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        &["created_at", "status"],
    )?;

    let contest_model = find_contest(&state.db, contest_id).await?;

    let can_view_all = auth_user.has_permission(perm::SUBMISSION_VIEW_ALL);

    // The kernel OWNS the subject: one kernel per request per subject. This
    // single instance also drives the per-row `apply_filter_to_list` pass
    // below.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // Gate contest visibility through `Resource::Contest` (the same gate
    // `get_contest`/`list_contest_problems` use, applying the
    // window/is_public/participant rule in `decide_contest`) so the activation
    // window (activate_time/deactivate_time) is enforced here exactly as on
    // every other contest read path. SUBMISSION_VIEW_ALL short-circuits it.
    // An `is_public` check alone is not enough: a *public but out-of-window*
    // contest (not yet activated, or deactivated/archived) would leak its full
    // submission list - usernames, verdicts, scores - to any authenticated
    // non-participant, and act as an existence oracle (200-with-data vs
    // 404). This top gate
    // decides `Resource::Contest`; the per-row pass below decides
    // `Resource::Submission` for each row - distinct resource kinds, so
    // there is no double-decision and this gate still runs, and still can
    // still 404, strictly before any row is fetched.
    if !can_view_all
        && kernel
            .decide(Action::Read, Resource::Contest(contest_id))
            .await?
            .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    // STATIC (plugin-independent) half of `visibility::host_rules::decide_submission`'s
    // non-owner branch: a peer's row is only even a candidate for this viewer
    // when the contest has `submissions_visible` AND the viewer is a
    // participant. The old `can_see_all` computed here checked only
    // `submissions_visible`, never participation - so a PUBLIC,
    // `submissions_visible` contest handed an authenticated NON-participant
    // the SQL of a full-access viewer: no `UserId` restriction on `total`,
    // while every row was then denied by the per-row kernel filter below
    // (`apply_filter_to_list`, which does check participation). The result
    // was `{"data": [], "pagination": {"total": <every submission in the
    // contest>}}` - an aggregate leaking exactly the count of rows the
    // viewer was denied. Folding participation in here closes that.
    //
    // This predicate is only NECESSARY for kernel-Allow, not sufficient: the
    // per-row kernel decision below can still Redact a row's content (e.g. a
    // plugin hiding fields), but it can never turn a row this predicate
    // excludes back into something visible, because a host `Deny` is final
    // (`Decision::meet(Deny, plugin) == Deny` - see
    // `visibility::VisibilityKernel::decide_batch`) and every row excluded
    // here is exactly a row `decide_submission` denies host-side for this
    // subject. So this predicate encodes every host rule, and `list_total`
    // below counts exactly what the viewer is shown unless a visibility
    // plugin may narrow it further.
    let can_see_all = if can_view_all {
        true
    } else if !contest_model.submissions_visible {
        // Short-circuits before the participation lookup: participation
        // alone never grants `can_see_all` (see `decide_submission` - both
        // conditions are required), so there is nothing to query for here.
        false
    } else {
        contest_user::Entity::find_by_id((contest_id, auth_user.user_id))
            .one(&state.db)
            .await?
            .is_some()
    };

    let per_page = PageWindow::page_size(query.per_page);

    let mut base_select =
        submission::Entity::find().filter(submission::Column::ContestId.eq(Some(contest_id)));

    if !can_see_all {
        base_select = base_select.filter(submission::Column::UserId.eq(auth_user.user_id));
    }

    if let Some(pid) = query.problem_id {
        base_select = base_select.filter(submission::Column::ProblemId.eq(pid));
    }
    if let Some(uid) = query.user_id
        && (can_see_all || uid == auth_user.user_id)
    {
        base_select = base_select.filter(submission::Column::UserId.eq(uid));
    }
    if let Some(ref lang) = query.language {
        base_select = base_select.filter(submission::Column::Language.eq(lang.trim()));
    }
    if let Some(status) = query.status {
        base_select = base_select.filter(
            crate::services::submission_feedback::public_status(can_view_all)
                .eq(status.to_string()),
        );
    }

    // Every row is in this contest, so only a querier governing its type
    // can hide one.
    let scope = ResourceScope::ContestType(contest_model.contest_type.clone());
    let total = list_total(&state, &kernel, &base_select, &scope).await?;

    // The page reuses the kernel of the `Resource::Contest` gate above. Each
    // row is decided as its own `Resource::Submission`, which also requires
    // contest participation for any row not covered by `submission:view_all`
    // or self-ownership, exactly as `get_submission` does (see
    // `visibility::host_rules::decide_submission`), so the list never shows a
    // row `get_submission` would 404 on.
    let order = list_page::ListOrder::from_query(
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        can_view_all,
    );
    let (data, pagination) = list_page::visible_page(
        &state,
        &kernel,
        base_select,
        &list_page::filtered_fields(&query),
        order,
        query.cursor.as_deref(),
        per_page,
        total,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "data": data,
        "pagination": pagination,
    })))
}

pub fn submission_body_limit(max_size: usize) -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(crate::upload_limits::source_request_limit_bytes(max_size))
}

#[cfg(test)]
mod tests {
    use super::response::submission_score_for_status;
    use common::SubmissionStatus;

    #[test]
    fn submission_score_is_visible_only_for_judged_status() {
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::Judged, Some(98.0)),
            Some(98.0)
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::CompilationError, Some(98.0)),
            None
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::SystemError, Some(98.0)),
            None
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::Running, Some(98.0)),
            None
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::Compiling, Some(98.0)),
            None
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::Pending, Some(98.0)),
            None
        );
        assert_eq!(
            submission_score_for_status(&SubmissionStatus::Queued, Some(98.0)),
            None
        );
    }
}
