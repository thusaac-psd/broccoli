use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use sea_orm::sea_query::LockType;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `list_users`/`get_user`/`update_user`/
// `delete_user`/`assign_role`/`revoke_role` all require perm::USER_MANAGE,
// pinned by `non_admin_cannot_list_all_users` and
// `user_cannot_modify_themselves_without_permission`
// (tests/integration/user.rs). `contest`/`contest_user` here are only used
// internally by `delete_user` to drop the deleted user's future contest
// registrations - never returned to a response body.
use crate::entity::{contest, contest_user, refresh_token, role, role_permission, user, user_role};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::models::user::{RoleAssignmentRequest, UpdateUserRequest, UserResponse};
use crate::state::AppState;
use crate::utils::hash;
use crate::utils::soft_delete::SoftDeletable;

#[utoipa::path(
    get,
    path = "/",
    tag = "Users",
    operation_id = "listUsers",
    summary = "List all users",
    description = "Returns all users with full stored fields. Requires `user:manage` permission.",
    responses(
        (status = 200, description = "List of users", body = Vec<UserResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(user_id = auth_user.user_id))]
pub async fn list_users(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserResponse>>, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let users = user::Entity::load()
        .filter(user::Column::DeletedAt.is_null())
        .order_by_asc(user::Column::Id)
        .with(role::Entity)
        .all(&state.db)
        .await?
        .into_iter()
        .map(UserResponse::from)
        .collect();

    Ok(Json(users))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Users",
    operation_id = "getUser",
    summary = "Get user details by ID",
    description = "Returns user details. Requires `user:manage` permission.",
    params(("id" = i32, Path, description = "User ID")),
    responses(
        (status = 200, description = "User details", body = UserResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn get_user(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<UserResponse>, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let user_model = user::Entity::load()
        .filter(user::Column::Id.eq(id))
        .with(role::Entity)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(UserResponse::from(user_model)))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Users",
    operation_id = "updateUser",
    summary = "Update user information",
    description = "Updates user information such as username and password. Requires `user:manage` permission.",
    params(("id" = i32, Path, description = "User ID")),
    request_body = UpdateUserRequest,
    responses(
        (status = 200, description = "Updated user details", body = UserResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn update_user(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<Json<UserResponse>, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let user_model = user::Entity::find_active_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let txn = state.db.begin().await?;

    let mut active: user::ActiveModel = user_model.into();
    if let Some(username) = payload.username {
        active.username = Set(username);
    }
    if let Some(password) = payload.password {
        let password_hash = hash::hash_password(&password)
            .map_err(|_| AppError::Validation("Failed to hash password".into()))?;
        active.password = Set(password_hash);
        refresh_token::Entity::revoke_all_for_user(&txn, id).await?;
        user::Entity::touch_credentials_changed(&txn, id).await?;
    }
    let updated_user = active.update(&txn).await?;

    txn.commit().await?;

    let user_with_roles = user::Entity::load()
        .filter(user::Column::Id.eq(updated_user.id))
        .with(role::Entity)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found after update".into()))?;

    Ok(Json(UserResponse::from(user_with_roles)))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Users",
    operation_id = "deleteUser",
    summary = "Soft-delete a user by ID",
    description = "Marks the user as deleted. The record is retained for historical data but the user can no longer log in and will not appear in listings. Requires `user:manage` permission.\n\nDeletion is blocked if the user is registered in a running or recently-ended contest. Registrations in future (not-yet-started) contests are automatically cancelled before deletion.",
    params(("id" = i32, Path, description = "User ID")),
    responses(
        (status = 204, description = "User deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "User is in an active or under-judgement contest (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id, user_id = auth_user.user_id))]
pub async fn delete_user(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let txn = state.db.begin().await?;

    let user_model = user::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let contest_ids: Vec<i32> = contest_user::Entity::find()
        .filter(contest_user::Column::UserId.eq(id))
        .select_only()
        .column(contest_user::Column::ContestId)
        .into_tuple()
        .all(&txn)
        .await?;

    let now = Utc::now();

    if !contest_ids.is_empty() {
        let contests = contest::Entity::find_active()
            .filter(contest::Column::Id.is_in(contest_ids))
            .all(&txn)
            .await?;

        for c in &contests {
            if c.start_time <= now && c.deactivate_time.is_none_or(|dt| dt > now) {
                return Err(AppError::Conflict(format!(
                    "Cannot delete user: currently participating in active contest \"{}\"",
                    c.title
                )));
            }
        }

        let future_ids: Vec<i32> = contests
            .iter()
            .filter(|c| c.start_time > now)
            .map(|c| c.id)
            .collect();

        if !future_ids.is_empty() {
            contest_user::Entity::delete_many()
                .filter(contest_user::Column::UserId.eq(id))
                .filter(contest_user::Column::ContestId.is_in(future_ids))
                .exec(&txn)
                .await?;
        }
    }

    let mut active: user::ActiveModel = user_model.into();
    active.deleted_at = Set(Some(now));
    active.update(&txn).await?;

    refresh_token::Entity::revoke_all_for_user(&txn, id).await?;

    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Privilege ceiling for adding or removing a role: a caller may only touch a role
/// whose permission set is a SUBSET of the caller's own (SYSTEM_ADMIN bypasses).
/// USER_MANAGE governs user accounts, not privilege definitions (that is
/// ROLE_MANAGE); without this ceiling a USER_MANAGE-only principal could assign
/// itself the all-powerful `admin` role (escalation) or strip `admin` from other
/// admins (lockout). Mirrors "you cannot grant or revoke what you do not hold".
async fn require_role_within_ceiling(
    state: &AppState,
    auth_user: &FreshAuthUser,
    role_name: &str,
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::SYSTEM_ADMIN) {
        return Ok(());
    }
    let role_permissions = role_permission::Entity::find()
        .filter(role_permission::Column::Role.eq(role_name.to_string()))
        .all(&state.db)
        .await?;
    if let Some(rp) = role_permissions
        .iter()
        .find(|rp| !auth_user.has_permission(&rp.permission))
    {
        tracing::warn!(
            actor = auth_user.user_id,
            role = %role_name,
            missing_permission = %rp.permission,
            "Refused role change crossing the caller's privilege ceiling"
        );
        return Err(AppError::PermissionDenied);
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/{id}/roles",
    tag = "Users",
    operation_id = "assignRole",
    summary = "Assign a role to a user",
    description = "Assigns a role to the user. Requires `user:manage` permission.",
    params(("id" = i32, Path, description = "User ID")),
    request_body = RoleAssignmentRequest,
    responses(
        (status = 201, description = "Role assigned"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn assign_role(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    Json(payload): Json<RoleAssignmentRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let role_model = role::Entity::find()
        .filter(role::Column::Name.eq(payload.role))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::Validation("Invalid role name".into()))?;

    let user_model = user::Entity::find_active_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    require_role_within_ceiling(&state, &auth_user, &role_model.name).await?;

    let txn = state.db.begin().await?;
    user_model.assign_role(&txn, role_model.name).await?;
    refresh_token::Entity::revoke_all_for_user(&txn, id).await?;
    user::Entity::touch_credentials_changed(&txn, id).await?;
    txn.commit().await?;

    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    delete,
    path = "/{id}/roles/{role}",
    tag = "Users",
    operation_id = "revokeRole",
    summary = "Revoke a role from a user",
    description = "Revokes a role from the user. Requires `user:manage` permission.",
    params(
        ("id" = i32, Path, description = "User ID"),
        ("role" = String, Path, description = "Role name")
    ),
    responses(
        (status = 204, description = "Role revoked"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User role not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn revoke_role(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((id, role_name)): AppPath<(i32, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    // Same privilege ceiling as assign_role: revoking a role also crosses the
    // USER_MANAGE/ROLE_MANAGE boundary. Without it, a USER_MANAGE-only principal
    // could strip `admin` (system:admin/role:manage) from every other admin - an
    // administrative lockout, the inverse of the self-escalation assign_role guards.
    require_role_within_ceiling(&state, &auth_user, &role_name).await?;

    let active = user_role::Entity::find_by_id((id, role_name))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User role not found".into()))?;

    // Revoke the user's refresh tokens in the same transaction as the role
    // delete, mirroring `assign_role`. Removing a role is a de-authorization, so
    // existing sessions must be forced to re-authenticate and re-read the reduced
    // role set rather than lingering on a stale cached permission.
    let txn = state.db.begin().await?;
    active.delete(&txn).await?;
    refresh_token::Entity::revoke_all_for_user(&txn, id).await?;
    user::Entity::touch_credentials_changed(&txn, id).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
