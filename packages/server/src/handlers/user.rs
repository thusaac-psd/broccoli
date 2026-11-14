use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::sea_query::LockType;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `list_users`/`get_user`/`update_user`/
// `delete_user`/`assign_role`/`revoke_role`/`create_user`/`bulk_create_users`
// all require perm::USER_MANAGE,
// pinned by `non_admin_cannot_list_all_users` and
// `user_cannot_modify_themselves_without_permission`
// (tests/integration/user.rs). `contest`/`contest_user` here are only used
// internally by `delete_user` to drop the deleted user's future contest
// registrations - never returned to a response body.
use crate::entity::{contest, contest_user, refresh_token, role, user, user_role};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::middleware::idempotency::NoStore;
use crate::models::user::{
    BulkCreateUsersRequest, CreateUserRequest, CreatedUserResponse, normalize_display_name,
    normalize_username, validate_create_users, validate_password,
};
use crate::models::user::{RoleAssignmentRequest, UpdateUserRequest, UserResponse};
use crate::services::privileges::{
    AdminSetChange, CeilingTarget, SystemAdminLock, require_within_ceiling,
};
use crate::state::AppState;
use crate::utils::contest::{ContestPhase, contest_phase, start_has_passed};
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
    description = "Updates username, display name, or password. Requires `user:manage` permission, including for one's own display name. Without `system:admin`, the target user may not hold any permission the caller lacks.",
    params(("id" = i32, Path, description = "User ID")),
    request_body = UpdateUserRequest,
    responses(
        (status = 200, description = "Updated user details", body = UserResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden: missing `user:manage`, or the target holds a permission the caller lacks (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Username taken (USERNAME_TAKEN)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn update_user(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    AppJson(payload): AppJson<UpdateUserRequest>,
) -> Result<Json<UserResponse>, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let username = payload
        .username
        .as_deref()
        .map(normalize_username)
        .transpose()?;
    let display_name = payload
        .display_name
        .as_deref()
        .map(normalize_display_name)
        .transpose()?;
    if let Some(password) = payload.password.as_deref() {
        validate_password(password)?;
    }

    // Hash before the transaction, so the row lock is never held across it.
    let password_hash = match payload.password {
        Some(password) => Some(
            hash::hash_password_off_thread(password)
                .await
                .map_err(|e| AppError::Internal(format!("Password hash error: {e}")))?,
        ),
        None => None,
    };

    let txn = state.db.begin().await?;
    let user_model = user::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    // Resetting a password is account takeover by another name: a
    // USER_MANAGE-only caller must not be able to log in as an admin.
    require_within_ceiling(&txn, &auth_user, CeilingTarget::User(id)).await?;

    let mut active: user::ActiveModel = user_model.into();
    let renamed_to = username.clone();
    if let Some(username) = username {
        active.username = Set(username);
    }
    if let Some(display_name) = display_name {
        active.display_name = Set(display_name);
    }
    if let Some(password_hash) = password_hash {
        active.password = Set(password_hash);
        refresh_token::Entity::revoke_all_for_user(&txn, id).await?;
        user::Entity::touch_credentials_changed(&txn, id).await?;
    }
    let updated_user =
        active
            .update(&txn)
            .await
            .map_err(crate::services::user_accounts::username_conflict(
                renamed_to.as_deref().unwrap_or_default(),
            ))?;

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
    description = "Marks the user as deleted. The record is retained for historical data but the user can no longer log in and will not appear in listings. Requires `user:manage` permission; without `system:admin`, the target user may not hold any permission the caller lacks.\n\nDeletion is blocked if the user is registered in a running or ended contest that is still active (not yet deactivated), or in a started contest that is not activated, or is the last active user holding `system:admin`. Registrations in contests that have not started or are not yet activated are automatically cancelled before deletion.",
    params(("id" = i32, Path, description = "User ID")),
    responses(
        (status = 204, description = "User deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden: missing `user:manage`, or the target holds a permission the caller lacks (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "User is in an active or under-judgement contest, or is the last active `system:admin` holder (CONFLICT)", body = ErrorBody),
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
    // Before the row lock, per the lock order in `services::privileges`.
    let admins = SystemAdminLock::acquire(&txn).await?;

    let user_model = user::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    // Deleting an account above the caller's ceiling would let a
    // USER_MANAGE-only principal remove every admin.
    require_within_ceiling(&txn, &auth_user, CeilingTarget::User(id)).await?;
    admins
        .require_survivor(AdminSetChange::DeleteUser { user_id: id })
        .await?;

    let contest_ids: Vec<i32> = contest_user::Entity::find()
        .filter(contest_user::Column::UserId.eq(id))
        .select_only()
        .column(contest_user::Column::ContestId)
        .into_tuple()
        .all(&txn)
        .await?;

    let now = state.clock.now();

    if !contest_ids.is_empty() {
        let contests = contest::Entity::find_active()
            .filter(contest::Column::Id.is_in(contest_ids))
            .all(&txn)
            .await?;

        let mut unstarted_ids = Vec::new();
        for c in &contests {
            match contest_phase(c, now) {
                // A running or ended-but-still-active contest may still be
                // judging and publishing standings.
                ContestPhase::Running | ContestPhase::Ended => {
                    return Err(AppError::Conflict(format!(
                        "Cannot delete user: currently participating in active contest \"{}\"",
                        c.title
                    )));
                }
                // A contest hidden after it ran may still publish standings
                // built from `contest_user`, so a started one blocks too.
                ContestPhase::Unactivated if start_has_passed(c, now) => {
                    return Err(AppError::Conflict(format!(
                        "Cannot delete user: participated in started contest \"{}\"",
                        c.title
                    )));
                }
                // No standings exist yet, so the registration goes with the
                // account.
                ContestPhase::Unactivated | ContestPhase::Upcoming => unstarted_ids.push(c.id),
                // Standings are built from `contest_user`: keep the row so
                // the participant's results survive.
                ContestPhase::Deactivated => {}
            }
        }

        if !unstarted_ids.is_empty() {
            contest_user::Entity::delete_many()
                .filter(contest_user::Column::UserId.eq(id))
                .filter(contest_user::Column::ContestId.is_in(unstarted_ids))
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

#[utoipa::path(
    post,
    path = "/{id}/roles",
    tag = "Users",
    operation_id = "assignRole",
    summary = "Assign a role to a user",
    description = "Assigns a role to the user. Requires `user:manage` permission. Idempotent: assigning a role the user already holds changes nothing and returns 200.",
    params(("id" = i32, Path, description = "User ID")),
    request_body = RoleAssignmentRequest,
    responses(
        (status = 200, description = "User already holds the role; nothing changed"),
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
    AppJson(payload): AppJson<RoleAssignmentRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;

    let role_model = role::Entity::find()
        .filter(role::Column::Name.eq(payload.role))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::Validation("Invalid role name".into()))?;

    if user::Entity::find_active_by_id(id)
        .one(&state.db)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound("User not found".into()));
    }

    // USER_MANAGE governs accounts, not privilege definitions: without the
    // ceiling its holder could assign itself the `admin` role.
    require_within_ceiling(&state.db, &auth_user, CeilingTarget::Role(&role_model.name)).await?;

    let txn = state.db.begin().await?;
    if !user_role::Entity::grant(&txn, id, &role_model.name).await? {
        // Already held: nothing changed, so existing sessions stay valid.
        return Ok(StatusCode::OK);
    }
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
    description = "Revokes a role from the user. Requires `user:manage` permission; without `system:admin`, neither the role nor the target user may hold a permission the caller lacks.\n\nRefused if afterwards no active user would hold `system:admin`.",
    params(
        ("id" = i32, Path, description = "User ID"),
        ("role" = String, Path, description = "Role name")
    ),
    responses(
        (status = 204, description = "Role revoked"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "User role not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Would leave no active `system:admin` holder (CONFLICT)", body = ErrorBody),
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
    require_within_ceiling(&state.db, &auth_user, CeilingTarget::Role(&role_name)).await?;

    // Revoke the user's refresh tokens in the same transaction as the role
    // delete, mirroring `assign_role`. Removing a role is a de-authorization, so
    // existing sessions must be forced to re-authenticate and re-read the reduced
    // role set rather than lingering on a stale cached permission.
    let txn = state.db.begin().await?;
    let admins = SystemAdminLock::acquire(&txn).await?;

    // Like update/delete: a caller may not act on a user above its ceiling,
    // even to remove a role it could itself hand out.
    user::Entity::find_active_by_id(id)
        .lock(LockType::Update)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User role not found".into()))?;
    require_within_ceiling(&txn, &auth_user, CeilingTarget::User(id)).await?;

    let active = user_role::Entity::find_by_id((id, role_name.clone()))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User role not found".into()))?;

    admins
        .require_survivor(AdminSetChange::RevokeUserRole {
            user_id: id,
            role: &role_name,
        })
        .await?;

    active.delete(&txn).await?;
    refresh_token::Entity::revoke_all_for_user(&txn, id).await?;
    user::Entity::touch_credentials_changed(&txn, id).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/", tag = "Users", operation_id = "createUser",
    summary = "Create a user",
    description = "Requires user:manage. Creates accounts with default roles and supplied or generated passwords. Bulk imports accept 1-100 entries and are atomic: a conflict or invalid entry creates no accounts. Existing accounts are never modified.",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "Users created with credentials", body = CreatedUserResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 409, description = "Username taken (USERNAME_TAKEN)", body = ErrorBody),
    ), security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(user_id = auth_user.user_id))]
pub async fn create_user(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<CreateUserRequest>,
) -> Result<(StatusCode, NoStore<Json<CreatedUserResponse>>), AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;
    let created = create_accounts(&state, vec![payload]).await?;
    Ok((
        StatusCode::CREATED,
        NoStore(Json(created.into_iter().next().ok_or_else(|| {
            AppError::Internal("Missing created account".into())
        })?)),
    ))
}

#[utoipa::path(
    post, path = "/bulk", tag = "Users", operation_id = "bulkCreateUsers",
    summary = "Create users in bulk",
    description = "Requires user:manage. Creates accounts with default roles and supplied or generated passwords. Bulk imports accept 1-100 entries and are atomic: a conflict or invalid entry creates no accounts. Existing accounts are never modified.",
    request_body = BulkCreateUsersRequest,
    responses(
        (status = 201, description = "Users created with credentials", body = Vec<CreatedUserResponse>),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 409, description = "Username taken (USERNAME_TAKEN)", body = ErrorBody),
    ), security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(user_id = auth_user.user_id))]
pub async fn bulk_create_users(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<BulkCreateUsersRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::USER_MANAGE)?;
    let created = create_accounts(&state, payload.users).await?;
    Ok((StatusCode::CREATED, NoStore(Json(created))))
}

async fn create_accounts(
    state: &AppState,
    users: Vec<CreateUserRequest>,
) -> Result<Vec<CreatedUserResponse>, AppError> {
    validate_create_users(&users)?;
    // Reject existing active usernames before doing expensive password
    // hashing, naming every requested one an account already holds.
    let taken: std::collections::HashSet<String> = user::Entity::find_active()
        .filter(user::username_is_any(users.iter().map(|u| &u.username)))
        .all(&state.db)
        .await?
        .iter()
        .map(|u| user::username_key(&u.username))
        .collect();
    if !taken.is_empty() {
        return Err(AppError::UsernameTaken(
            users
                .iter()
                .map(|u| u.username.trim().to_owned())
                .filter(|name| taken.contains(&user::username_key(name)))
                .collect(),
        ));
    }
    let prepared = crate::services::user_accounts::prepare_accounts(users).await?;
    let txn = state.db.begin().await?;
    let mut created = Vec::with_capacity(prepared.len());
    for entry in prepared {
        let user = crate::services::user_accounts::insert_account(&txn, &entry).await?;
        created.push(CreatedUserResponse {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            created_at: user.created_at,
            roles: role::DEFAULT_ROLES
                .iter()
                .map(|r| (*r).to_owned())
                .collect(),
            password: entry.password,
        });
    }
    txn.commit().await?;
    Ok(created)
}
