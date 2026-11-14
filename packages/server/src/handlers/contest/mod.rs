use crate::extractors::query::AppQuery;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use common::db::DatabaseTransaction;
use sea_orm::prelude::Expr;
use sea_orm::sea_query::{Func, LikeExpr, Query as SeaQuery};
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `create_contest`/`update_contest`/`delete_contest`
// require perm::CONTEST_CREATE/CONTEST_MANAGE/CONTEST_DELETE respectively,
// asserted at handler entry and pinned by tests/integration/contest.rs's
// `contestant_cannot_create_a_contest`, `contestant_cannot_update_contest`,
// and `contestant_cannot_delete_a_contest`. The two single-contest reads,
// `get_contest`/`get_contest_my_info`, gate through
// `kernel.decide(Action::Read, Resource::Contest(id)).is_denied()`
// (`visibility::host_rules::decide_contest` applies the
// window/is_public/participant rule), so a contest-scoped plugin's `Deny` on
// `Resource::Contest` takes effect on these two endpoints exactly as it does
// on `list_contest_problems` and the clarification read paths; pinned by
// `visibility_plugin::contest_level_gates_all_route_through_the_kernel`.
// `find_contest` (via `find_active_by_id`) runs BEFORE the kernel gate, so a
// soft-deleted contest is 404 even for a `contest:manage` admin - pinned by
// `visibility_matrix::soft_delete::contest_soft_deleted_is_not_found_even_for_contest_manage_admin`.
// `list_contests` narrows a viewer without perm::CONTEST_MANAGE to public
// or enrolled contests in SQL and to contests inside their activation window
// with `utils::contest::contest_phase`, the one definition of the window,
// before paging (the admin view pages in SQL), pinned by
// `contestant_sees_only_public_and_enrolled_contests`,
// `contestant_does_not_see_never_activating_contests`,
// `contestant_does_not_see_not_yet_activated_contests`, and
// `contestant_does_not_see_deactivated_contests` - then also runs every
// page (bounded to `per_page`, max 100) through
// `kernel.decide_batch(Action::Read, ..)` and drops any row the kernel
// denies, so the same future contest-scoped plugin narrowing applies here
// too. The kernel pass can only shrink a page the window filter selected,
// never add a row back. No bundled plugin registers for `Resource::Contest`, so with the
// bundled plugins these kernel gates answer exactly as the host rule does -
// the same as every other `Resource::Contest` gate in this codebase (see
// `handlers/clarification.rs`). `list_participants`
// (handlers/contest/participants.rs) and `list_contest_submissions`'s top
// gate (handlers/submission/mod.rs) are documented separately at their own
// call sites.
use crate::entity::{contest, contest_user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::contest::*;
use crate::models::problem::validate_contest_type;
use crate::models::shared::{PageWindow, escape_like};
use crate::services::plugin_config::{
    ConfigTarget, ConfigTargetPattern, delete_config_by_target, delete_config_by_target_pattern,
};
use crate::state::AppState;
use crate::utils::contest::find_contest;
use crate::utils::soft_delete::SoftDeletable;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

mod participants;
mod problems;
mod samples;

pub use participants::*;
pub use problems::*;
pub use samples::*;

#[utoipa::path(
    post,
    path = "/",
    tag = "Contests",
    operation_id = "createContest",
    summary = "Create a new contest",
    description = "Creates a new contest. Requires `contest:create` permission.",
    request_body = CreateContestRequest,
    responses(
        (status = 201, description = "Contest created", body = ContestResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(title = %payload.title))]
pub async fn create_contest(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<CreateContestRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_CREATE)?;
    validate_create_contest(&payload)?;
    validate_contest_type(
        "contest_type",
        &payload.contest_type,
        &state.registries.contest_type_registry,
    )
    .await?;

    let now = state.clock.now();
    let new_contest = contest::ActiveModel {
        title: Set(payload.title.trim().to_string()),
        description: Set(payload.description),
        activate_time: Set(payload.activate_time.unwrap_or(None)),
        start_time: Set(payload.start_time),
        end_time: Set(payload.end_time),
        deactivate_time: Set(payload.deactivate_time.unwrap_or(None)),
        is_public: Set(payload.is_public),
        submissions_visible: Set(payload.submissions_visible.unwrap_or(false)),
        show_compile_output: Set(payload.show_compile_output.unwrap_or(true)),
        show_participants_list: Set(payload.show_participants_list.unwrap_or(true)),
        contest_type: Set(Some(payload.contest_type)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let model = new_contest.insert(&state.db).await?;

    Ok((StatusCode::CREATED, Json(contest_response(model, now))))
}
#[utoipa::path(
    get,
    path = "/",
    tag = "Contests",
    operation_id = "listContests",
    summary = "List contests with pagination and search",
    description = "Returns a paginated list of contests with optional search and sorting. Users with `contest:manage` see all contests; others only see active public contests and those they are enrolled in. Supports sorting by `created_at`, `updated_at`, `activate_time`, `start_time`, or `title`.",
    params(ContestListQuery),
    responses(
        (status = 200, description = "List of contests", body = ContestListResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn list_contests(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppQuery(query): AppQuery<ContestListQuery>,
) -> Result<Json<ContestListResponse>, AppError> {
    let window = PageWindow::new(query.page, query.per_page)?;

    let mut select = contest::Entity::find_active();
    let now = state.clock.now();
    // Everyone but `contest:manage` sees only contests inside their activation
    // window, which only `contest_phase` decides: those rows are filtered and
    // paged here, after the query. A deployment holds tens of contests, so
    // reading all of a viewer's candidates costs one small query.
    let manager = auth_user.has_permission(perm::CONTEST_MANAGE);
    if !manager {
        select = select.filter(
            Condition::any()
                .add(contest::Column::IsPublic.eq(true))
                .add(
                    contest::Column::Id.in_subquery(
                        SeaQuery::select()
                            .column(contest_user::Column::ContestId)
                            .from(contest_user::Entity)
                            .and_where(contest_user::Column::UserId.eq(auth_user.user_id))
                            .to_owned(),
                    ),
                ),
        );
    }

    if let Some(ref search) = query.search {
        let term = escape_like(search.trim());
        if !term.is_empty() {
            select = select.filter(
                Expr::expr(Func::lower(Expr::col(contest::Column::Title)))
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
        "created_at" => contest::Column::CreatedAt,
        "updated_at" => contest::Column::UpdatedAt,
        "activate_time" => contest::Column::ActivateTime,
        "start_time" => contest::Column::StartTime,
        "title" => contest::Column::Title,
        _ => {
            return Err(AppError::Validation(
                "sort_by must be one of: created_at, updated_at, activate_time, start_time, title"
                    .into(),
            ));
        }
    };

    select = select.order_by_with_nulls(sort_column, sort_order, sea_query::NullOrdering::Last);

    let (data, total) = if manager {
        let total = select.clone().count(&state.db).await?;
        let page = select
            .offset(Some(window.offset()))
            .limit(Some(window.limit()))
            .into_partial_model::<ContestListRow>()
            .all(&state.db)
            .await?;
        (page, total)
    } else {
        let active: Vec<ContestListRow> = select
            .into_partial_model::<ContestListRow>()
            .all(&state.db)
            .await?
            .into_iter()
            .filter(|row| row.phase(now).is_active())
            .collect();
        let total = active.len() as u64;
        let page = active
            .into_iter()
            .skip(usize::try_from(window.offset()).unwrap_or(usize::MAX))
            .take(usize::try_from(window.limit()).unwrap_or(usize::MAX))
            .collect();
        (page, total)
    };

    // Kernel narrowing pass over this page only (bounded by `per_page`,
    // clamped above to at most 100) - see the module-level comment for why
    // it can only shrink the page. The kernel OWNS the subject: one kernel
    // per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    let resources: Vec<Resource> = data.iter().map(|c| Resource::Contest(c.id)).collect();
    let decisions = kernel.decide_batch(Action::Read, &resources).await?;
    let data: Vec<ContestListItem> = data
        .into_iter()
        .zip(decisions)
        .filter_map(|(item, decision)| {
            if decision.is_denied() {
                None
            } else {
                Some(item.into_item(now))
            }
        })
        .collect();

    Ok(Json(ContestListResponse {
        data,
        pagination: window.pagination(total),
    }))
}
#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Contests",
    operation_id = "getContest",
    summary = "Get a contest by ID",
    description = "Returns the full details of a contest. Users with `contest:manage` can view any contest; others can view active public contests or those they are enrolled in. Returns 404 (not 403) for inaccessible contests to prevent enumeration.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 200, description = "Contest details", body = ContestResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_contest(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<ContestResponse>, AppError> {
    let model = find_contest(&state.db, id).await?;

    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    if kernel
        .decide(Action::Read, Resource::Contest(id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    Ok(Json(contest_response(model, state.clock.now())))
}
#[utoipa::path(
    get,
    path = "/{id}/me",
    tag = "Contests",
    operation_id = "getContestMyInfo",
    summary = "Get current user's contest context",
    description = "Returns contest-related information for the authenticated user in the specified contest. Uses the same contest visibility rules as getContest.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 200, description = "Current user's contest context", body = ContestUserContextResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_contest_my_info(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<ContestUserContextResponse>, AppError> {
    // `find_contest` resolves via `find_active_by_id`, so a soft-deleted
    // contest 404s here first, before the kernel gate below - see the
    // module-level comment.
    find_contest(&state.db, id).await?;

    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));
    if kernel
        .decide(Action::Read, Resource::Contest(id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let registration = contest_user::Entity::find_by_id((id, auth_user.user_id))
        .one(&state.db)
        .await?;

    Ok(Json(ContestUserContextResponse {
        contest_id: id,
        user_id: auth_user.user_id,
        is_registered: registration.is_some(),
        registered_at: registration.map(|m| m.registered_at),
    }))
}
#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Contests",
    operation_id = "updateContest",
    summary = "Update an existing contest",
    description = "Partially updates a contest using PATCH semantics. Requires `contest:manage` permission. An empty payload returns the current resource unchanged. Cross-field validation ensures activate_time <= start_time < end_time <= deactivate_time (if deactivate_time is set) even when fields are updated independently. Returns 404 if the contest does not exist.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = UpdateContestRequest,
    responses(
        (status = 200, description = "Contest updated", body = ContestResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "The contest type cannot change once the contest has a submission (CONTEST_TYPE_LOCKED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(id))]
pub async fn update_contest(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    AppJson(payload): AppJson<UpdateContestRequest>,
) -> Result<Json<ContestResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_update_contest(&payload)?;
    if let Some(ref contest_type) = payload.contest_type {
        validate_contest_type(
            "contest_type",
            contest_type,
            &state.registries.contest_type_registry,
        )
        .await?;
    }

    if payload == UpdateContestRequest::default() {
        let existing = find_contest(&state.db, id).await?;
        return Ok(Json(contest_response(existing, state.clock.now())));
    }

    let txn = state.db.begin().await?;
    let existing = find_contest_for_update(&txn, id).await?;
    if let Some(ref contest_type) = payload.contest_type
        && existing.contest_type.as_deref() != Some(contest_type.as_str())
        && crate::utils::contest::contest_has_submissions(&txn, id).await?
    {
        return Err(AppError::ContestTypeLocked);
    }

    validate_contest_timeline(
        payload.activate_time.unwrap_or(existing.activate_time),
        payload.start_time.unwrap_or(existing.start_time),
        payload.end_time.unwrap_or(existing.end_time),
        payload.deactivate_time.unwrap_or(existing.deactivate_time),
    )?;

    let mut active: contest::ActiveModel = existing.into();

    if let Some(ref title) = payload.title {
        active.title = Set(title.trim().to_string());
    }
    if let Some(description) = payload.description {
        active.description = Set(description);
    }
    if let Some(activate_time) = payload.activate_time {
        active.activate_time = Set(activate_time);
    }
    if let Some(start_time) = payload.start_time {
        active.start_time = Set(start_time);
    }
    if let Some(end_time) = payload.end_time {
        active.end_time = Set(end_time);
    }
    if let Some(deactivate_time) = payload.deactivate_time {
        active.deactivate_time = Set(deactivate_time);
    }
    if let Some(is_public) = payload.is_public {
        active.is_public = Set(is_public);
    }
    if let Some(submissions_visible) = payload.submissions_visible {
        active.submissions_visible = Set(submissions_visible);
    }
    if let Some(show_compile_output) = payload.show_compile_output {
        active.show_compile_output = Set(show_compile_output);
    }
    if let Some(show_participants_list) = payload.show_participants_list {
        active.show_participants_list = Set(show_participants_list);
    }
    if let Some(contest_type) = payload.contest_type {
        active.contest_type = Set(Some(contest_type));
    }
    active.updated_at = Set(state.clock.now());

    let model = active.update(&txn).await?;
    txn.commit().await?;

    Ok(Json(contest_response(model, state.clock.now())))
}
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Contests",
    operation_id = "deleteContest",
    summary = "Soft-delete a contest by ID",
    description = "Marks a contest as deleted without removing historical submissions or participant records. Requires `contest:delete` permission.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 204, description = "Contest deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn delete_contest(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_DELETE)?;

    let txn = state.db.begin().await?;
    let contest = find_contest_for_update(&txn, id).await?;

    let mut active: contest::ActiveModel = contest.into();
    active.deleted_at = Set(Some(state.clock.now()));
    active.update(&txn).await?;

    delete_config_by_target(&txn, &ConfigTarget::contest(id)).await?;
    delete_config_by_target_pattern(&txn, &ConfigTargetPattern::contest_problem_by_contest(id))
        .await?;

    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn find_contest_for_update(
    txn: &DatabaseTransaction,
    id: i32,
) -> Result<contest::Model, AppError> {
    use sea_orm::sea_query::LockType;
    contest::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Contest not found".into()))
}
