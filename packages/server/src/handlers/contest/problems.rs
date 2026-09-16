use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::prelude::Expr;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: the admin writes (`add_contest_problem`,
// `update_contest_problem`, `remove_contest_problem`, `reorder_contest_problems`,
// `bulk_delete_contest_problems`) all require perm::CONTEST_MANAGE, pinned by
// `contestant_cannot_add_problem_to_contest`, `contestant_cannot_update_contest_problem`,
// `contestant_cannot_remove_contest_problem`, and
// `contestant_cannot_reorder_contest_problems` (tests/integration/contest.rs).
// The one viewer-facing read, `list_contest_problems`, already routes through
// `VisibilityKernel` below (`Resource::Contest` then `Resource::Problem` per
// row via `fetch_visible_batch`); these entity types are only used for the
// pre-kernel row fetch that builds the (Resource, DTO) pairs the kernel then
// decides on - the same pattern as `handlers/submission/mod.rs`.
use crate::entity::{contest_problem, problem};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::contest::*;
use crate::services::plugin_config::{ConfigTarget, delete_config_by_target};
use crate::state::AppState;
use crate::utils::contest::{find_contest, find_contest_problem, require_contest_started};
use crate::utils::soft_delete::SoftDeletable;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

use super::find_contest_for_update;

#[utoipa::path(
    post,
    path = "/",
    tag = "Contest Problems",
    operation_id = "addContestProblem",
    summary = "Add a problem to a contest",
    description = "Associates an existing problem with the contest under a given label. Requires `contest:manage` permission. Labels must be unique within the contest. Position is auto-assigned if omitted. Returns 409 if the problem ID or label is already present.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = AddContestProblemRequest,
    responses(
        (status = 201, description = "Problem added to contest", body = ContestProblemResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Problem already in contest (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id))]
pub async fn add_contest_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<AddContestProblemRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_add_contest_problem(&payload)?;

    let txn = state.db.begin().await?;
    let _contest = find_contest_for_update(&txn, contest_id).await?;

    let problem_model = problem::Entity::find_active_by_id(payload.problem_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;
    let problem_title = problem_model.title;

    if contest_problem::Entity::find_by_id((contest_id, payload.problem_id))
        .one(&txn)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(
            "Problem is already in this contest".into(),
        ));
    }

    let label = payload.label.trim().to_string();
    let existing_label = contest_problem::Entity::find()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .filter(contest_problem::Column::Label.eq(&label))
        .one(&txn)
        .await?;
    if existing_label.is_some() {
        return Err(AppError::Conflict(format!(
            "Label '{label}' is already used in this contest"
        )));
    }

    let position = match payload.position {
        Some(p) => p,
        None => next_problem_position(&txn, contest_id).await?,
    };

    let new_cp = contest_problem::ActiveModel {
        contest_id: Set(contest_id),
        problem_id: Set(payload.problem_id),
        label: Set(label),
        position: Set(position),
    };

    let model = new_cp.insert(&txn).await?;
    txn.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(contest_problem_response(model, problem_title)),
    ))
}
#[utoipa::path(
    get,
    path = "/",
    tag = "Contest Problems",
    operation_id = "listContestProblems",
    summary = "List problems in a contest",
    description = "Returns all problems in the contest, ordered by position. Same visibility rules as getContest apply.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 200, description = "List of contest problems", body = Vec<ContestProblemResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id))]
pub async fn list_contest_problems(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject. `decide`
    // and `decide_batch` therefore take no `subject` argument.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // NOTE: this is a pure reachability gate on the contest itself, distinct
    // from the `Resource::Problem` decisions made below (which DO go
    // through `fetch_visible_batch`/`into_masked_json` and so honor
    // `Redact` correctly). `is_denied()` treats a hypothetical `Redact` on
    // this `Resource::Contest` decision the same as `Allow`, and this
    // decision's contest is never rendered from here, so `Redact`
    // degenerates to `Allow`. No plugin currently returns `Redact` for
    // `Resource::Contest`, so this is a documented no-op today, not a live
    // bug - see Task 20 Item 6.
    if kernel
        .decide(Action::Read, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    // The kernel's Contest decision already folds in the activation-window
    // gate (`check_contest_access` + `require_contest_started`'s window
    // predicate - see `visibility::host_rules`). It deliberately does NOT
    // cover `require_contest_started`'s `now < start_time` business-rule
    // rejection (a 400, not a reachability outcome), so that check is
    // re-applied here on top of the kernel's `Allow`.
    let contest_model = find_contest(&state.db, contest_id).await?;
    require_contest_started(&auth_user, &contest_model)?;

    let rows = contest_problem::Entity::find()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .find_also_related(problem::Entity)
        .order_by_asc(contest_problem::Column::Position)
        .all(&state.db)
        .await?;

    let items: Vec<(Resource, ContestProblemResponse)> = rows
        .into_iter()
        .map(|(cp, prob)| {
            let resource = Resource::Problem {
                contest_id: Some(contest_id),
                problem_id: cp.problem_id,
            };
            let dto = contest_problem_response(cp, prob.map(|p| p.title).unwrap_or_default());
            (resource, dto)
        })
        .collect();

    let visible = kernel.fetch_visible_batch(Action::Read, items).await?;

    // A denied problem is omitted from the list, never rendered as a
    // placeholder - a placeholder would confirm it exists, the exact fact a
    // staged-release contest format is hiding. Every surviving DTO passes
    // through `into_masked_json`, so a `Redact` decision cannot be
    // forgotten at serialization time.
    let body: Vec<serde_json::Value> = visible
        .into_iter()
        .flatten()
        .map(|v| v.into_masked_json())
        .collect::<Result<_, _>>()?;

    Ok(Json(body))
}
#[utoipa::path(
    patch,
    path = "/{problem_id}",
    tag = "Contest Problems",
    operation_id = "updateContestProblem",
    summary = "Update a contest problem's label or position",
    description = "Updates the label or position of a problem within a contest. Requires `contest:manage` permission. Returns 409 CONFLICT on duplicate labels.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    request_body = UpdateContestProblemRequest,
    responses(
        (status = 200, description = "Contest problem updated", body = ContestProblemResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Duplicate label in contest (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id, problem_id))]
pub async fn update_contest_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<UpdateContestProblemRequest>,
) -> Result<Json<ContestProblemResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_update_contest_problem(&payload)?;

    if payload == UpdateContestProblemRequest::default() {
        let cp = find_contest_problem(&state.db, contest_id, problem_id).await?;
        let title = problem::Entity::find_by_id(problem_id)
            .one(&state.db)
            .await?
            .map(|p| p.title)
            .unwrap_or_default();
        return Ok(Json(contest_problem_response(cp, title)));
    }

    let txn = state.db.begin().await?;
    let _contest = find_contest_for_update(&txn, contest_id).await?;
    let existing = find_contest_problem(&txn, contest_id, problem_id).await?;

    if let Some(ref new_label) = payload.label {
        let label = new_label.trim();
        if label != existing.label {
            let dup = contest_problem::Entity::find()
                .filter(contest_problem::Column::ContestId.eq(contest_id))
                .filter(contest_problem::Column::Label.eq(label))
                .one(&txn)
                .await?;
            if dup.is_some() {
                return Err(AppError::Conflict(format!(
                    "Label '{label}' is already used in this contest"
                )));
            }
        }
    }

    let mut active: contest_problem::ActiveModel = existing.into();

    if let Some(ref label) = payload.label {
        active.label = Set(label.trim().to_string());
    }
    if let Some(position) = payload.position {
        active.position = Set(position);
    }

    let model = active.update(&txn).await?;
    let title = problem::Entity::find_by_id(model.problem_id)
        .one(&txn)
        .await?
        .map(|p| p.title)
        .unwrap_or_default();
    txn.commit().await?;

    Ok(Json(contest_problem_response(model, title)))
}
#[utoipa::path(
    delete,
    path = "/{problem_id}",
    tag = "Contest Problems",
    operation_id = "removeContestProblem",
    summary = "Remove a problem from a contest",
    description = "Removes the association between a problem and the contest. Requires `contest:manage` permission. The problem itself is not deleted.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    responses(
        (status = 204, description = "Problem removed from contest"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, problem_id))]
pub async fn remove_contest_problem(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;
    let cp = find_contest_problem(&txn, contest_id, problem_id).await?;
    let active: contest_problem::ActiveModel = cp.into();
    active.delete(&txn).await?;

    delete_config_by_target(&txn, &ConfigTarget::contest_problem(contest_id, problem_id)).await?;

    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    put,
    path = "/reorder",
    tag = "Contest Problems",
    operation_id = "reorderContestProblems",
    summary = "Reorder problems in a contest",
    description = "Replaces the ordering of all problems in a contest. Requires `contest:manage` permission. The ID array must contain exactly all problems currently in the contest. Positions are assigned by array index starting at 0.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = ReorderContestProblemsRequest,
    responses(
        (status = 204, description = "Contest problems reordered"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id))]
pub async fn reorder_contest_problems(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<ReorderContestProblemsRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_reorder_contest_problems(&payload)?;

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;

    let existing: Vec<i32> = contest_problem::Entity::find()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .select_only()
        .column(contest_problem::Column::ProblemId)
        .into_tuple::<i32>()
        .all(&txn)
        .await?;

    let existing_set: std::collections::HashSet<i32> = existing.into_iter().collect();
    let payload_set: std::collections::HashSet<i32> = payload.problem_ids.iter().copied().collect();
    if existing_set != payload_set {
        return Err(AppError::Validation(
            "problem_ids must contain exactly the problems currently in the contest".into(),
        ));
    }

    for (i, &problem_id) in payload.problem_ids.iter().enumerate() {
        contest_problem::Entity::update_many()
            .filter(contest_problem::Column::ContestId.eq(contest_id))
            .filter(contest_problem::Column::ProblemId.eq(problem_id))
            .col_expr(
                contest_problem::Column::Position,
                Expr::value(
                    i32::try_from(i)
                        .map_err(|_| AppError::Validation("Too many problems to reorder".into()))?,
                ),
            )
            .exec(&txn)
            .await?;
    }

    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    delete,
    path = "/bulk",
    tag = "Contest Problems",
    operation_id = "bulkDeleteContestProblems",
    summary = "Bulk-remove problems from a contest",
    description = "Removes multiple problems from a contest in a single operation. Requires `contest:manage` permission. All provided problem IDs must be currently in the contest.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = BulkDeleteContestProblemsRequest,
    responses(
        (status = 200, description = "Problems removed", body = BulkDeleteContestProblemsResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found or problem IDs not in contest (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id))]
pub async fn bulk_delete_contest_problems(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<BulkDeleteContestProblemsRequest>,
) -> Result<Json<BulkDeleteContestProblemsResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_bulk_delete_contest_problems(&payload)?;

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;

    let existing_ids: Vec<i32> = contest_problem::Entity::find()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .filter(contest_problem::Column::ProblemId.is_in(payload.problem_ids.clone()))
        .select_only()
        .column(contest_problem::Column::ProblemId)
        .into_tuple::<i32>()
        .all(&txn)
        .await?;

    let existing_set: std::collections::HashSet<i32> = existing_ids.into_iter().collect();
    let missing: Vec<i32> = payload
        .problem_ids
        .iter()
        .filter(|id| !existing_set.contains(id))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(AppError::NotFound(format!(
            "Problem IDs not found in contest {contest_id}: {missing:?}"
        )));
    }

    // Mirror remove_contest_problem's per-association config cleanup. Every other
    // path that destroys a contest_problem link deletes the link's
    // per-contest-problem plugin config too: single remove_contest_problem here,
    // delete_contest via the contest_problem_by_contest pattern, and delete_problem
    // via contest_problem_by_problem. Bulk delete skipped it, orphaning that config
    // - and because config is keyed by (contest_id, problem_id), re-adding the same
    // problem to the contest would silently resurrect the stale, admin-removed
    // config. Every id here is already validated to be in the contest above.
    for &problem_id in &payload.problem_ids {
        delete_config_by_target(&txn, &ConfigTarget::contest_problem(contest_id, problem_id))
            .await?;
    }

    let result = contest_problem::Entity::delete_many()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .filter(contest_problem::Column::ProblemId.is_in(payload.problem_ids))
        .exec(&txn)
        .await?;

    txn.commit().await?;

    tracing::info!(
        contest_id,
        removed = result.rows_affected,
        user_id = auth_user.user_id,
        "Bulk removed contest problems"
    );

    Ok(Json(BulkDeleteContestProblemsResponse {
        removed: result.rows_affected as usize,
    }))
}
fn contest_problem_response(
    cp: contest_problem::Model,
    problem_title: String,
) -> ContestProblemResponse {
    ContestProblemResponse {
        contest_id: cp.contest_id,
        problem_id: cp.problem_id,
        label: cp.label,
        position: cp.position,
        problem_title,
    }
}
async fn next_problem_position<C: ConnectionTrait>(
    db: &C,
    contest_id: i32,
) -> Result<i32, AppError> {
    let max_pos: Option<i32> = contest_problem::Entity::find()
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .select_only()
        .column_as(contest_problem::Column::Position.max(), "max_pos")
        .into_tuple::<Option<i32>>()
        .one(db)
        .await?
        .flatten();
    max_pos
        .unwrap_or(-1)
        .checked_add(1)
        .ok_or_else(|| AppError::Validation("Position overflow".into()))
}
