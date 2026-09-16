use std::cmp;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::types::BeforeSubmissionEvent;
use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::*;
use tracing::instrument;

use crate::dispatcher::queue_depth::enforce_queue_depth_admission;
// visibility-bypass-audited: every read handler in this module already
// routes through `VisibilityKernel` (`get_submission`/`list_submission_judgements`
// via `Resource::Submission`, `list_submissions`/`list_contest_submissions`
// via `fetch_visible_batch`, `create_submission`/`create_contest_submission`
// via `Resource::Problem` before accepting a submit) - these entity types are
// only used for the pre-kernel row fetch and for `list_contest_submissions`'s
// deliberately-retained `check_contest_access` contest-reachability gate (see
// the comment at that call site, and `handlers/contest/mod.rs` for the same
// audited pattern), pinned by the frozen
// `tests/integration/visibility_matrix.rs::contest_submission_list` and
// `submission_detail` suites.
use crate::entity::{contest, contest_user, problem, submission, submission_judgement, user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::hooks;
use crate::models::shared::{Pagination, escape_like};
use crate::models::submission::*;
use crate::state::AppState;
use crate::utils::contest::{
    check_contest_access, find_contest, require_contest_participant, require_contest_running,
};
use crate::utils::judging::{files_to_json, validate_code_payload, validate_submission_contract};
use crate::utils::problem::find_problem;
use crate::utils::query::validate_sorting_params;
use crate::utils::rate_limit::check_rate_limit;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

mod dispatch;
mod filter;
mod rejudge;
mod response;

pub use rejudge::*;

// Re-exported for the DLQ retry path, which is an immediate rejudge and must
// prepare the judgement lineage through the same primitive as the admin
// rejudge endpoints (see `crate::handlers::dlq`).
pub(crate) use dispatch::open_rejudge_judgement;

use dispatch::{dispatch_before_submission_hooks, find_submission, fire_after_submission_hooks};
use filter::{apply_filter_to_judgement_response, apply_filter_to_list, apply_filter_to_response};
use response::{
    VisibilityContext, build_judgement_response, build_submission_list_items,
    build_submission_response,
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
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
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
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // UP#39 backpressure-on-post: shed load before touching the DB so
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
    // REACHABILITY FIRST: gate on problem read access (contest membership or
    // problem-edit permission), same as viewing the problem, and do it before
    // the `before_submission` hook dispatch below. Without this a contestant
    // can probe and submit against hidden/unreleased problems by guessing
    // IDs, which is a stronger information oracle than viewing since it runs
    // secret tests. A denied decision fails closed and silent (404) - this is
    // reachability, not a business rule, so it must never grow a reason
    // string; the cooldown/submission-limit plugins' own loud, reasoned
    // rejections are a separate, later check untouched by this gate. See
    // `visibility::host_rules::decide_standalone_problem_access`, ported
    // verbatim from the `require_problem_read_access` this replaces.
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
        None => problem.default_contest_type.clone(),
    };

    let hook_event = BeforeSubmissionEvent {
        user_id: auth_user.user_id,
        problem_id,
        contest_id: None,
        language: payload.language.trim().to_string(),
        file_count: payload.files.len(),
    };
    let enabled_plugins = hooks::fetch_resource_enablements(problem_id, None, &state.db).await?;
    dispatch_before_submission_hooks(&state, &hook_event, Some(&enabled_plugins)).await?;

    let now = Utc::now();
    let language = payload.language.trim().to_string();
    let new_submission = submission::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language.clone()),
        // UP#37: persist `Queued` and return 201 immediately. The
        // per-server claim fiber (UP#38, see
        // `dispatcher/claim.rs`) promotes the row to `Pending` and
        // dispatches it. Replacing the previous `tokio::spawn(dispatch_to_plugin)`
        // closes the silent-loss window where an api crash between
        // commit and spawn would lose the submission with no MQ
        // message and no recoverable state.
        status: Set(SubmissionStatus::Queued),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(None),
        contest_type: Set(contest_type),
        created_at: Set(now),
        ..Default::default()
    };

    let model = new_submission.insert(&state.db).await?;

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
    let response =
        build_submission_response(&state.db, &*state.blob_store, model, visibility).await?;

    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Submissions",
    operation_id = "listSubmissions",
    summary = "List submissions",
    description = "Returns a paginated list of submissions. Users see their own submissions; users with `submission:view_all` permission see all submissions.",
    params(SubmissionListQuery),
    responses(
        (status = 200, description = "List of submissions", body = SubmissionListResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
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

    let page = cmp::max(query.page.unwrap_or(1), 1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);

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
        base_select = base_select.filter(submission::Column::Status.eq(status));
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

    // KNOWN RESIDUAL LEAK, accepted: `total` (and therefore `total_pages`)
    // is computed from `base_select` BEFORE the per-row kernel filter below
    // runs, i.e. it counts rows this SQL predicate matches, not rows the
    // viewer will actually be shown. For a caller whose page ends up
    // narrower than this count, the response DOES disclose two things: that
    // at least one more matching submission exists beyond what `data`
    // contains, and the exact count of such submissions. It discloses
    // NOTHING about their CONTENT - no ids, users, verdicts, code, or
    // contest identity leak through this number, only a count.
    //
    // Task 17 / Step 2b (I2): the mechanism that can cause this divergence
    // is plugin-level narrowing via `Decision::meet` below, NOT
    // `decide_submission`'s participation / `submissions_visible` branch -
    // that branch is unreachable from this endpoint. Every row `base_select`
    // can return is already host-`Allow` before the kernel is even asked:
    // a non-`submission:view_all` caller's query is filtered to
    // `UserId.eq(auth_user.user_id)` above, which hits `decide_submission`'s
    // unconditional owner bypass, and a `submission:view_all` caller hits its
    // permission bypass - both `Allow` unconditionally, never reaching the
    // participation/`submissions_visible` check at all. A registered
    // visibility plugin can still `Deny` a host-`Allow`ed row via
    // `Decision::meet` (`meet(Allow, Deny) == Deny`), and `apply_filter_to_list`
    // drops denied rows outright rather than rendering a placeholder - that
    // plugin-level narrowing is the only source of `total` vs `data.len()`
    // divergence here.
    //
    // This is NOT fixed the way `list_contest_submissions` is fixed below,
    // because there is no single-contest static predicate to push into SQL
    // here: this endpoint is GLOBAL and unscoped, one page can span many
    // contests plus contest-less submissions, and which plugin (if any) would
    // narrow a given row depends on THAT row's own contest's registered
    // contest type - not one fixed predicate known up front. Making `total`
    // exact would mean running the full kernel decision (a plugin round trip
    // for every row) over every row the filters match in the WHOLE TABLE, not
    // just the current page - unbounded work per list request, scaling with
    // total submissions rather than `per_page`. That cost is why this leak is
    // left in place rather than closed; `list_contest_submissions` is scoped
    // to one contest and can hoist the static part of its own
    // (`decide_submission`-derived) rule into `WHERE`, so it does not have
    // this excuse.
    let total = base_select.clone().count(&state.db).await?;

    let select = base_select.find_also_related(user::Entity);

    let sort_order = if query.sort_order.as_deref() == Some("asc") {
        Order::Asc
    } else {
        Order::Desc
    };

    let select = match query.sort_by.as_deref().unwrap_or("created_at") {
        "created_at" => select.order_by(submission::Column::CreatedAt, sort_order),
        "status" => select.order_by(submission::Column::Status, sort_order),
        _ => select.order_by(submission::Column::CreatedAt, Order::Desc),
    };

    let submissions = select
        .offset(Some((page - 1) * per_page))
        .limit(Some(per_page))
        .all(&state.db)
        .await?;

    let data = build_submission_list_items(&state.db, submissions).await?;
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    // This list is GLOBAL - it can span several contests plus contest-less
    // submissions in one page. One `decide_batch` (via `fetch_visible_batch`)
    // over every listed submission's own `Resource::Submission(id)` resolves
    // each row's own contest scope internally (`host_decide`'s
    // `submission_contest_ids` out-param), so no extra plumbing is needed
    // here to keep rows from different contests from bleeding into each
    // other's decision.
    let data = apply_filter_to_list(&kernel, data).await?;
    let total_pages = total.div_ceil(per_page);

    Ok(Json(serde_json::json!({
        "data": data,
        "pagination": Pagination {
            page,
            per_page,
            total,
            total_pages,
        },
    })))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Submissions",
    operation_id = "getSubmission",
    summary = "Get submission details",
    description = "Returns full details of a submission. Users can view their own submissions; users with `submission:view_all` permission can view any submission.",
    params(
        ("id" = i32, Path, description = "Submission ID")
    ),
    responses(
        (status = 200, description = "Submission details", body = SubmissionResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Submission not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(submission_id = %id))]
pub async fn get_submission(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let resource = Resource::Submission(id);

    // Fail fast, before building the full detail response (user/problem/
    // contest/judgement/test-case-result reads below), on a submission the
    // kernel already knows is unreachable. This single decision folds in
    // what `require_submission_visible` used to check by hand - owner
    // bypass, the contest activation window, participation, and
    // `submissions_visible` - see `visibility::host_rules::decide_submission`,
    // a byte-for-byte port of that old logic.
    if kernel.decide(Action::Read, resource).await?.is_denied() {
        return Err(AppError::NotFound("Submission not found".into()));
    }

    let sub = find_submission(&state.db, id).await?;
    let visibility = Some(VisibilityContext::from_auth_user(&auth_user));
    let response =
        build_submission_response(&state.db, &*state.blob_store, sub, visibility).await?;

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
    let contest_ended = contest_model
        .as_ref()
        .is_none_or(|c| Utc::now() > c.end_time);
    let show_compile_output = visibility.has_view_all
        || is_owner
        || contest_ended
        || contest_model
            .as_ref()
            .is_some_and(|c| c.show_compile_output);
    let show_test_details = visibility.has_view_all || problem_model.show_test_details;

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
        let response = build_judgement_response(
            &state.db,
            &*state.blob_store,
            judgement,
            show_compile_output,
            show_test_details,
        )
        .await?;
        let response = apply_filter_to_judgement_response(
            &kernel,
            &sub,
            &user_model,
            &problem_model,
            response,
            &visibility,
        )
        .await?;
        responses.push(response);
    }

    Ok(Json(responses))
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
        (status = 503, description = "Durable queue depth exceeded (QUEUE_OVERLOADED)", body = ErrorBody),
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
        auth_user.user_id,
        state.config.submission.rate_limit_per_minute,
    )
    .await?;
    // UP#39 backpressure-on-post: same rationale as `create_submission`.
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

    let now = Utc::now();
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

    let enabled_plugins =
        hooks::fetch_resource_enablements(problem_id, Some(contest_id), &state.db).await?;
    let hook_event = BeforeSubmissionEvent {
        user_id: auth_user.user_id,
        problem_id,
        contest_id: Some(contest_id),
        language: payload.language.trim().to_string(),
        file_count: payload.files.len(),
    };
    dispatch_before_submission_hooks(&state, &hook_event, Some(&enabled_plugins)).await?;

    let language = payload.language.trim().to_string();
    let contest_type = match &contest_model.contest_type {
        Some(ct) => ct.clone(),
        None => {
            let reg = state.registries.contest_type_registry.read().await;
            reg.keys().min().cloned().unwrap_or_default()
        }
    };
    let new_submission = submission::ActiveModel {
        files: Set(files_to_json(&payload.files)),
        language: Set(language.clone()),
        // UP#37: see the contest-free `create_submission` handler for the
        // full rationale - `Queued` is the durable-accept state the claim
        // fiber transitions to `Pending`.
        status: Set(SubmissionStatus::Queued),
        user_id: Set(auth_user.user_id),
        problem_id: Set(problem_id),
        contest_id: Set(Some(contest_id)),
        contest_type: Set(contest_type),
        created_at: Set(now),
        ..Default::default()
    };

    let model = new_submission.insert(&state.db).await?;

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
    let response =
        build_submission_response(&state.db, &*state.blob_store, model, visibility).await?;

    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Submissions",
    operation_id = "listContestSubmissions",
    summary = "List contest submissions",
    description = "Returns submissions for a contest.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        SubmissionListQuery
    ),
    responses(
        (status = 200, description = "List of submissions", body = SubmissionListResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
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

    // Gate contest visibility through the shared access check so the activation
    // window (activate_time/deactivate_time) is enforced here exactly as on every
    // other contest read path. SUBMISSION_VIEW_ALL still short-circuits it, matching
    // the prior behaviour where that permission bypassed the gate. The hand-rolled
    // `is_public` check this replaces skipped the window entirely: a *public but
    // out-of-window* contest (not yet activated, or deactivated/archived) leaked its
    // full submission list - usernames, verdicts, scores - to any authenticated
    // non-participant, and acted as an existence oracle (200-with-data vs 404).
    if !can_view_all {
        check_contest_access(&state.db, &auth_user, &contest_model).await?;
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
    // subject. So `total`, computed against this predicate below, can never
    // undercount what the per-row filter would allow - it stays a safe upper
    // bound, just a far tighter one than counting the whole contest.
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

    let page = cmp::max(query.page.unwrap_or(1), 1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);

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
        base_select = base_select.filter(submission::Column::Status.eq(status));
    }

    let total = base_select.clone().count(&state.db).await?;

    let select = base_select.find_also_related(user::Entity);

    let sort_order = if query.sort_order.as_deref() == Some("asc") {
        Order::Asc
    } else {
        Order::Desc
    };

    let select = match query.sort_by.as_deref().unwrap_or("created_at") {
        "created_at" => select.order_by(submission::Column::CreatedAt, sort_order),
        "status" => select.order_by(submission::Column::Status, sort_order),
        _ => select.order_by(submission::Column::CreatedAt, Order::Desc),
    };

    let submissions = select
        .offset(Some((page - 1) * per_page))
        .limit(Some(per_page))
        .all(&state.db)
        .await?;

    let data = build_submission_list_items(&state.db, submissions).await?;
    // The kernel OWNS the subject: one kernel per request per subject. The
    // top-level `check_contest_access` gate above is untouched - it stays the
    // sole reachability check for the contest itself, `submission:view_all`
    // (not `contest:manage`) is still what bypasses it. This per-row decision
    // is an ADDITIONAL, independent narrowing on top: it also requires
    // contest participation for any row not already covered by
    // `submission:view_all` or self-ownership, matching `get_submission`'s
    // row-level behaviour exactly (see `visibility::host_rules::decide_submission`).
    // Previously the plugin-based filter had no way to omit a row at all, so
    // every viewer who passed the top-level gate saw every row the SQL
    // query returned; a genuinely non-participant viewer of a public,
    // `submissions_visible` contest could see peers' submissions in the list
    // that `get_submission` would already 404 on individually - this closes
    // that inconsistency.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let data = apply_filter_to_list(&kernel, data).await?;
    let total_pages = total.div_ceil(per_page);

    Ok(Json(serde_json::json!({
        "data": data,
        "pagination": Pagination {
            page,
            per_page,
            total,
            total_pages,
        },
    })))
}

pub fn submission_body_limit(max_size: usize) -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(max_size + 4096)
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
