use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use common::db::DatabaseTransaction;
use sea_orm::prelude::Expr;
use sea_orm::sea_query::{Func, LikeExpr};
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `create_problem`/`update_problem`/`delete_problem`
// require perm::PROBLEM_CREATE/PROBLEM_EDIT/PROBLEM_DELETE, and `list_problems`
// (the admin problem-bank browser, not the contestant-facing problem list)
// requires PROBLEM_CREATE or PROBLEM_EDIT - all asserted at handler entry and
// pinned by tests/integration/problem.rs's permission-denial tests for each
// verb. The viewer-facing reads, `get_problem` and `get_contest_problem`,
// route through `VisibilityKernel` below (`Resource::Problem`);
// `contest`/`contest_problem` here are only used for the pre-kernel row fetch
// and for the admin write paths' own bookkeeping.
use crate::entity::{contest, contest_problem, problem, test_case};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::problem::*;
use crate::models::shared::PageWindow;
use crate::services::plugin_config::{
    ConfigTarget, ConfigTargetPattern, delete_config_by_target, delete_config_by_target_pattern,
};
use crate::state::AppState;
use crate::utils::contest::{find_contest, require_contest_started};
use crate::utils::problem::find_problem;
use crate::utils::soft_delete::SoftDeletable;
use crate::utils::test_case_body::test_case_body_size;
use crate::utils::text::{sanitize_db_json, sanitize_db_text};
use crate::visibility::{Action, ProblemScope, Resource, Subject, VisibilityKernel};

mod checker_source;
mod test_cases;
mod upload;

pub use checker_source::*;
pub use test_cases::*;
pub use upload::*;

#[utoipa::path(
    post,
    path = "/",
    tag = "Problems",
    operation_id = "createProblem",
    summary = "Create a new problem",
    description = "Creates a new problem in the system. Requires `problem:create` permission.",
    request_body = CreateProblemRequest,
    responses(
        (status = 201, description = "Problem created", body = ProblemResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(title = %payload.title))]
pub async fn create_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<CreateProblemRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_CREATE)?;
    validate_create_problem(&payload)?;

    let problem_type = payload.problem_type;
    let default_contest_type = payload.default_contest_type;

    validate_problem_type(&problem_type, &state.registries.evaluator_registry).await?;
    validate_checker_format(
        &payload.checker_format,
        &state.registries.checker_stage_registry,
    )
    .await?;
    let known_languages = state.registries.known_language_ids().await;
    let submission_format = payload
        .submission_format
        .as_ref()
        .map(|sf| parse_submission_format(sf, &known_languages))
        .transpose()?;
    validate_contest_type(
        "default_contest_type",
        &default_contest_type,
        &state.registries.contest_type_registry,
    )
    .await?;

    let now = chrono::Utc::now();
    let submission_format_json = submission_format
        .map(|sf| sanitize_db_json(serde_json::to_value(sf).unwrap_or(serde_json::Value::Null)));
    let new_problem = problem::ActiveModel {
        title: Set(sanitize_db_text(payload.title.trim())),
        content: Set(sanitize_db_text(payload.content)),
        time_limit: Set(payload.time_limit),
        memory_limit: Set(payload.memory_limit),
        problem_type: Set(problem_type),
        checker_format: Set(payload.checker_format),
        default_contest_type: Set(default_contest_type),
        show_test_details: Set(payload.show_test_details.unwrap_or(false)),
        is_public: Set(payload.is_public.unwrap_or(false)),
        submission_format: Set(submission_format_json),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let model = new_problem.insert(&state.db).await?;

    Ok((StatusCode::CREATED, Json(ProblemResponse::from(model))))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Problems",
    operation_id = "listProblems",
    summary = "List problems with pagination and search",
    description = "Returns a paginated list of problems with optional search and sorting. Requires `problem:create` or `problem:edit` permission. Supports case-insensitive title search and sorting by `created_at` (default, desc), `updated_at`, or `title`. Problem content is omitted from list results.",
    params(ProblemListQuery),
    responses(
        (status = 200, description = "List of problems", body = ProblemListResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn list_problems(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ProblemListQuery>,
) -> Result<Json<ProblemListResponse>, AppError> {
    auth_user.require_any_permission(&[perm::PROBLEM_CREATE, perm::PROBLEM_EDIT])?;

    let window = PageWindow::new(query.page, query.per_page)?;

    let mut select = problem::Entity::find_active();

    if let Some(ref search) = query.search {
        let term = escape_like(search.trim());
        if !term.is_empty() {
            select = select.filter(
                Expr::expr(Func::lower(Expr::col(problem::Column::Title)))
                    .like(LikeExpr::new(format!("%{}%", term.to_lowercase())).escape('\\')),
            );
        }
    }

    let sort_by = query.sort_by.as_deref().unwrap_or("created_at");
    let sort_order = if query.sort_order.as_deref() == Some("asc") {
        Order::Asc
    } else {
        Order::Desc
    };
    let sort_column = match sort_by {
        "created_at" => problem::Column::CreatedAt,
        "updated_at" => problem::Column::UpdatedAt,
        "title" => problem::Column::Title,
        _ => {
            return Err(AppError::Validation(
                "sort_by must be one of: created_at, updated_at, title".into(),
            ));
        }
    };

    let total = select.clone().count(&state.db).await?;

    select = select.order_by(sort_column, sort_order);

    let data = select
        .offset(Some(window.offset()))
        .limit(Some(window.limit()))
        .into_partial_model::<ProblemListItem>()
        .all(&state.db)
        .await?;

    Ok(Json(ProblemListResponse {
        data,
        pagination: window.pagination(total),
    }))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Problems",
    operation_id = "getProblem",
    summary = "Get a problem by ID",
    description = "Returns the full details of a problem, including its Markdown content and sample test case metadata. Accessible to users with `problem:create`/`problem:edit` permission, or to participants of any active (started) contest that includes this problem.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "Problem details", body = ProblemResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_problem(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    get_problem_in(&state, &auth_user, ProblemScope::standalone(id)).await
}

#[utoipa::path(
    get,
    path = "/{problem_id}",
    tag = "Contest Problems",
    operation_id = "getContestProblem",
    summary = "Get a contest problem",
    description = "Returns the same problem details as `getProblem`, read through this contest. Visibility is decided for this contest only, so a problem that another contest still hides stays readable here. The contest must have started.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    responses(
        (status = 200, description = "Problem details", body = ProblemResponse),
        (status = 400, description = "Contest has not started yet (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, problem_id))]
pub async fn get_contest_problem(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    get_problem_in(
        &state,
        &auth_user,
        ProblemScope::in_contest(contest_id, problem_id),
    )
    .await
}

/// The reachability gate every viewer-facing read of a problem part (its
/// statement, samples or attachments) passes before touching any row, in an
/// order that tells the viewer nothing they could not already see:
///
/// 1. under a contest, the contest itself must be reachable (404) and must
///    have started (400). These are `list_contest_problems`' two gates in
///    the same order, so before the start a 400 says only that the contest
///    has not started, never whether this problem is one of its problems;
/// 2. the problem must be reachable under `scope` (404).
///
/// `not_found` is the calling handler's own 404 message. The "not started"
/// rule is a 400 business rule, not a reachability outcome, so the kernel
/// does not model it and it is applied here between the two decisions.
pub(crate) async fn require_problem_part_reachable(
    kernel: &VisibilityKernel<'_>,
    state: &AppState,
    auth_user: &AuthUser,
    scope: ProblemScope,
    action: Action,
    not_found: &str,
) -> Result<(), AppError> {
    if let Some(contest_id) = scope.contest_id {
        if kernel
            .decide(Action::Read, Resource::Contest(contest_id))
            .await?
            .is_denied()
        {
            return Err(AppError::NotFound(not_found.into()));
        }
        let contest = find_contest(&state.db, contest_id).await?;
        require_contest_started(auth_user, &contest, chrono::Utc::now())?;
    }
    if kernel.decide(action, scope.problem()).await?.is_denied() {
        return Err(AppError::NotFound(not_found.into()));
    }
    Ok(())
}

/// Shared body of `get_problem` and `get_contest_problem`: the problem as
/// reached under `scope`.
async fn get_problem_in(
    state: &AppState,
    auth_user: &AuthUser,
    scope: ProblemScope,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(state, Subject::from_auth_user(auth_user));
    let resource = scope.problem();

    // Fail fast, before doing any of the reads below, on a resource the
    // kernel already knows is unreachable. Standalone, this folds in what
    // the pre-kernel handler checked as `require_problem_read_access`
    // (permission bypass, `is_public`, and - for a hidden draft -
    // reachability via any contest it is attached to); under a contest it
    // is that contest's access, window and membership checks - see
    // `visibility::host_rules::decide_problem_or_sample`.
    require_problem_part_reachable(
        &kernel,
        state,
        auth_user,
        scope,
        Action::Read,
        "Problem not found",
    )
    .await?;

    let mut response = ProblemResponse::from(find_problem(&state.db, scope.problem_id).await?);
    response.samples = load_sample_test_cases(&state.db, scope.problem_id).await?;

    // The DTO reaches the response body only through `into_masked_json`, so
    // a `Redact` decision on this Problem resource can never be forgotten at
    // the serialization step. The kernel memoizes per (Action, Resource), so
    // this re-decides the same `resource` already `Allow`ed above at no
    // extra DB cost, and the `None` arm is unreachable in practice (it was
    // already `Allow`, not `Deny`) but kept honest rather than `.unwrap()`-ed
    // away.
    let visible = kernel
        .fetch_visible(Action::Read, resource, response)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;

    Ok(Json(visible.into_masked_json()?))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Problems",
    operation_id = "updateProblem",
    summary = "Update an existing problem",
    description = "Partially updates a problem using PATCH semantics — only provided fields are modified. Requires `problem:edit` permission. An empty payload returns the current resource unchanged.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = UpdateProblemRequest,
    responses(
        (status = 200, description = "Problem updated", body = ProblemResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(id))]
pub async fn update_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    AppJson(payload): AppJson<UpdateProblemRequest>,
) -> Result<Json<ProblemResponse>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;
    validate_update_problem(&payload)?;
    if let Some(ref pt) = payload.problem_type {
        validate_problem_type(pt, &state.registries.evaluator_registry).await?;
    }
    if let Some(ref cf) = payload.checker_format {
        validate_checker_format(cf, &state.registries.checker_stage_registry).await?;
    }
    // Parsed (normalised) once here; the parsed value is what gets stored.
    let submission_format = match &payload.submission_format {
        Some(Some(sf)) => {
            let known_languages = state.registries.known_language_ids().await;
            Some(Some(parse_submission_format(sf, &known_languages)?))
        }
        Some(None) => Some(None),
        None => None,
    };
    if let Some(ref ct) = payload.default_contest_type {
        validate_contest_type(
            "default_contest_type",
            ct,
            &state.registries.contest_type_registry,
        )
        .await?;
    }

    if payload == UpdateProblemRequest::default() {
        let mut existing = ProblemResponse::from(find_problem(&state.db, id).await?);
        existing.samples = load_sample_test_cases(&state.db, id).await?;
        return Ok(Json(existing));
    }

    let txn = state.db.begin().await?;

    let existing = find_problem(&txn, id).await?;
    let mut active: problem::ActiveModel = existing.into();

    if let Some(ref title) = payload.title {
        active.title = Set(sanitize_db_text(title.trim()));
    }
    if let Some(content) = payload.content {
        active.content = Set(sanitize_db_text(content));
    }
    if let Some(tl) = payload.time_limit {
        active.time_limit = Set(tl);
    }
    if let Some(ml) = payload.memory_limit {
        active.memory_limit = Set(ml);
    }
    if let Some(problem_type) = payload.problem_type {
        active.problem_type = Set(problem_type);
    }
    if let Some(checker_format) = payload.checker_format {
        active.checker_format = Set(checker_format);
    }
    if let Some(default_contest_type) = payload.default_contest_type {
        active.default_contest_type = Set(default_contest_type);
    }
    if let Some(show_test_details) = payload.show_test_details {
        active.show_test_details = Set(show_test_details);
    }
    if let Some(is_public) = payload.is_public {
        active.is_public = Set(is_public);
    }
    match submission_format {
        Some(Some(sf)) => {
            active.submission_format = Set(Some(sanitize_db_json(
                serde_json::to_value(sf).unwrap_or(serde_json::Value::Null),
            )));
        }
        Some(None) => {
            active.submission_format = Set(None);
        }
        None => {}
    }
    active.updated_at = Set(chrono::Utc::now());

    let model = active.update(&txn).await?;
    txn.commit().await?;

    let mut response = ProblemResponse::from(model);
    response.samples = load_sample_test_cases(&state.db, id).await?;
    Ok(Json(response))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Problems",
    operation_id = "deleteProblem",
    summary = "Soft-delete a problem by ID",
    description = "Marks a problem as deleted without removing historical data. Requires `problem:delete` permission. Returns 409 CONFLICT if the problem is currently part of a contest.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 204, description = "Problem deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Cannot delete: part of a contest (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn delete_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_DELETE)?;

    let txn = state.db.begin().await?;

    let problem = find_problem_for_update(&txn, id).await?;

    let contest_ids_with_problem: Vec<i32> = contest_problem::Entity::find()
        .filter(contest_problem::Column::ProblemId.eq(id))
        .select_only()
        .column(contest_problem::Column::ContestId)
        .into_tuple()
        .all(&txn)
        .await?;

    if !contest_ids_with_problem.is_empty() {
        let active_contest_count = contest::Entity::find_active()
            .filter(contest::Column::Id.is_in(contest_ids_with_problem))
            .count(&txn)
            .await?;
        if active_contest_count > 0 {
            return Err(AppError::Conflict(
                "Cannot delete problem associated with a contest".into(),
            ));
        }
    }

    let mut active: problem::ActiveModel = problem.into();
    active.deleted_at = Set(Some(chrono::Utc::now()));
    active.update(&txn).await?;

    delete_config_by_target(&txn, &ConfigTarget::problem(id)).await?;
    delete_config_by_target_pattern(&txn, &ConfigTargetPattern::contest_problem_by_problem(id))
        .await?;

    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn load_sample_test_cases<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
) -> Result<Vec<SampleTestCaseMeta>, AppError> {
    let rows = test_case::Entity::find()
        .filter(test_case::Column::ProblemId.eq(problem_id))
        .filter(test_case::Column::IsSample.eq(true))
        .order_by_asc(test_case::Column::Position)
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|tc| SampleTestCaseMeta {
            id: tc.id,
            input_size: test_case_body_size(&tc.input, tc.input_size),
            output_size: test_case_body_size(&tc.expected_output, tc.expected_output_size),
            description: tc.description,
        })
        .collect())
}

pub(super) async fn find_problem_for_update(
    txn: &DatabaseTransaction,
    id: i32,
) -> Result<problem::Model, AppError> {
    use sea_orm::sea_query::LockType;
    problem::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))
}
