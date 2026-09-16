use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::prelude::Expr;
use sea_orm::sea_query::{Func, LikeExpr, Query as SeaQuery};
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `create_contest`/`update_contest`/`delete_contest`
// require perm::CONTEST_CREATE/CONTEST_MANAGE/CONTEST_DELETE respectively,
// asserted at handler entry and pinned by tests/integration/contest.rs's
// `contestant_cannot_create_a_contest`, `contestant_cannot_update_contest`,
// and `contestant_cannot_delete_a_contest`. The two single-contest reads,
// `get_contest`/`get_contest_my_info`, gate through `check_contest_access`
// (window + is_public + participant), the same host rule the kernel's
// `decide_contest` is a direct port of (see
// `visibility::host_rules::decide_contest`) - `check_contest_access` is kept
// as the sole reachability check here, mirroring the deliberate,
// already-reviewed precedent in `list_contest_submissions`
// (handlers/submission/mod.rs) - and is independently unit-tested in
// `utils::contest::contest_access_tests`, plus pinned end-to-end by
// `non_participant_cannot_see_private_contest`. `list_contests` filters the
// same window/is_public/participant conditions at the SQL layer with a
// perm::CONTEST_MANAGE bypass for the admin view, pinned by
// `contestant_sees_only_public_and_enrolled_contests`,
// `contestant_does_not_see_never_activating_contests`,
// `contestant_does_not_see_not_yet_activated_contests`, and
// `contestant_does_not_see_deactivated_contests`.
use crate::entity::{contest, contest_user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::contest::*;
use crate::models::shared::{Pagination, escape_like};
use crate::services::plugin_config::{
    ConfigTarget, ConfigTargetPattern, delete_config_by_target, delete_config_by_target_pattern,
};
use crate::state::AppState;
use crate::utils::contest::{check_contest_access, find_contest};
use crate::utils::soft_delete::SoftDeletable;
use crate::utils::text::sanitize_db_text;

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

    let now = chrono::Utc::now();
    let new_contest = contest::ActiveModel {
        title: Set(sanitize_db_text(payload.title.trim())),
        description: Set(sanitize_db_text(payload.description)),
        activate_time: Set(payload.activate_time.unwrap_or(None)),
        start_time: Set(payload.start_time),
        end_time: Set(payload.end_time),
        deactivate_time: Set(payload.deactivate_time.unwrap_or(None)),
        is_public: Set(payload.is_public),
        submissions_visible: Set(payload.submissions_visible.unwrap_or(false)),
        show_compile_output: Set(payload.show_compile_output.unwrap_or(true)),
        show_participants_list: Set(payload.show_participants_list.unwrap_or(true)),
        contest_type: Set(payload.contest_type),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let model = new_contest.insert(&state.db).await?;

    Ok((StatusCode::CREATED, Json(ContestResponse::from(model))))
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
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn list_contests(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<ContestListQuery>,
) -> Result<Json<ContestListResponse>, AppError> {
    let page = Ord::max(query.page.unwrap_or(1), 1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);

    let mut select = contest::Entity::find_active();

    if !auth_user.has_permission(perm::CONTEST_MANAGE) {
        let now = chrono::Utc::now();
        select = select
            .filter(
                Condition::all()
                    .add(
                        contest::Column::ActivateTime
                            .is_not_null()
                            .and(contest::Column::ActivateTime.lte(now)),
                    )
                    .add(
                        contest::Column::DeactivateTime
                            .is_null()
                            .or(contest::Column::DeactivateTime.gt(now)),
                    ),
            )
            .filter(
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

    let total = select
        .clone()
        .paginate(&state.db, per_page)
        .num_items()
        .await?;

    select = select.order_by_with_nulls(sort_column, sort_order, sea_query::NullOrdering::Last);
    let total_pages = total.div_ceil(per_page);

    let data = select
        .select_only()
        .column(contest::Column::Id)
        .column(contest::Column::Title)
        .column(contest::Column::ActivateTime)
        .column(contest::Column::StartTime)
        .column(contest::Column::EndTime)
        .column(contest::Column::DeactivateTime)
        .column(contest::Column::IsPublic)
        .column(contest::Column::SubmissionsVisible)
        .column(contest::Column::ShowCompileOutput)
        .column(contest::Column::ShowParticipantsList)
        .column(contest::Column::CreatedAt)
        .column(contest::Column::UpdatedAt)
        .offset(Some((page - 1) * per_page))
        .limit(Some(per_page))
        .into_model::<ContestListItem>()
        .all(&state.db)
        .await?;

    Ok(Json(ContestListResponse {
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
    check_contest_access(&state.db, &auth_user, &model).await?;
    Ok(Json(model.into()))
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
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_contest_my_info(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<ContestUserContextResponse>, AppError> {
    let model = find_contest(&state.db, id).await?;
    check_contest_access(&state.db, &auth_user, &model).await?;

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

    if payload == UpdateContestRequest::default() {
        let existing = find_contest(&state.db, id).await?;
        return Ok(Json(existing.into()));
    }

    let txn = state.db.begin().await?;
    let existing = find_contest_for_update(&txn, id).await?;

    validate_contest_timeline(
        payload.activate_time.unwrap_or(existing.activate_time),
        payload.start_time.unwrap_or(existing.start_time),
        payload.end_time.unwrap_or(existing.end_time),
        payload.deactivate_time.unwrap_or(existing.deactivate_time),
    )?;

    let mut active: contest::ActiveModel = existing.into();

    if let Some(ref title) = payload.title {
        active.title = Set(sanitize_db_text(title.trim()));
    }
    if let Some(description) = payload.description {
        active.description = Set(sanitize_db_text(description));
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
    active.updated_at = Set(chrono::Utc::now());

    let model = active.update(&txn).await?;
    txn.commit().await?;

    Ok(Json(model.into()))
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
    active.deleted_at = Set(Some(chrono::Utc::now()));
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
