use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::*;

// visibility-bypass-audited: every handler in this module requires
// perm::ROLE_MANAGE, pinned by
// tests/integration/user.rs::regular_user_cannot_access_role_permissions
// (mod role_management). This is RBAC administration, never a per-row
// Contest/Problem/Submission/Clarification view the kernel governs.
use crate::entity::{role, role_permission, user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::user::PermissionGrantRequest;
use crate::services::privileges::{
    AdminSetChange, CeilingTarget, SystemAdminLock, require_within_ceiling,
};
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "/",
    tag = "Roles",
    operation_id = "listRoles",
    summary = "List all roles",
    description = "Returns a list of all roles. Requires `role:manage` permission.",
    responses(
        (status = 200, description = "List of roles", body = Vec<String>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn list_roles(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::ROLE_MANAGE)?;

    let roles: Vec<String> = role::Entity::find()
        .all(&state.db)
        .await?
        .into_iter()
        .map(|r| r.name)
        .collect();

    Ok(Json(roles))
}

#[utoipa::path(
    get,
    path = "/{role}/permissions",
    tag = "Roles",
    operation_id = "listRolePermissions",
    summary = "List permissions granted to a role",
    description = "Returns a list of permissions granted to the specified role. Requires `role:manage` permission.",
    params(("role" = String, Path, description = "Role name")),
    responses(
        (status = 200, description = "List of permissions", body = Vec<String>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn list_role_permissions(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(role_name): AppPath<String>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::ROLE_MANAGE)?;

    let permissions: Vec<String> = role_permission::Entity::find()
        .filter(role_permission::Column::Role.eq(role_name))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|rp| rp.permission)
        .collect();

    Ok(Json(permissions))
}

#[utoipa::path(
    post,
    path = "/{role}/permissions",
    tag = "Roles",
    operation_id = "grantPermissionToRole",
    summary = "Grant a permission to a role",
    description = "Grants a permission to a role. Requires `role:manage` permission, and, unless it holds `system:admin`, the caller must itself hold the permission being granted and every permission the role already has.",
    params(("role" = String, Path, description = "Role name")),
    request_body = PermissionGrantRequest,
    responses(
        (status = 201, description = "Permission granted successfully"),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED): missing `role:manage`, or the permission or role is above the caller's ceiling", body = ErrorBody),
        (status = 404, description = "Role not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn grant_permission_to_role(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(role_name): AppPath<String>,
    AppJson(req): AppJson<PermissionGrantRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::ROLE_MANAGE)?;

    // Grant + invalidate the in-flight access tokens of everyone holding this
    // role in one transaction, so the new permission cannot be exercised by a
    // stale token from before the grant.
    let txn = state.db.begin().await?;

    // Roles are not created through the API, so an unknown role is a 404
    // rather than a foreign-key failure on the insert below. It comes before
    // the ceiling: role names are no secret to a `role:manage` holder
    // (`GET /roles` lists them), and the ceiling of a role that does not
    // exist is meaningless.
    if role::Entity::find_by_id(role_name.clone())
        .one(&txn)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound("Role not found".into()));
    }

    // `role:manage` is not root-equivalent; without the ceiling its holder
    // could grant `system:admin` to its own role.
    require_within_ceiling(
        &txn,
        &auth_user,
        CeilingTarget::RolePermission {
            role: &role_name,
            permission: &req.permission,
        },
    )
    .await?;

    let existing = role_permission::Entity::find_by_id((role_name.clone(), req.permission.clone()))
        .one(&txn)
        .await?;
    if existing.is_some() {
        return Err(AppError::Validation(
            "Permission already granted to role".into(),
        ));
    }

    role_permission::ActiveModel {
        role: Set(role_name.clone()),
        permission: Set(req.permission),
    }
    .insert(&txn)
    .await?;
    user::Entity::touch_credentials_changed_for_role(&txn, &role_name).await?;
    txn.commit().await?;

    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    delete,
    path = "/{role}/permissions/{permission}",
    tag = "Roles",
    operation_id = "revokePermissionFromRole",
    summary = "Revoke a permission from a role",
    description = "Revokes a permission from a role. Requires `role:manage` permission, and, unless it holds `system:admin`, the caller must itself hold every permission of the role, including the one being revoked.\n\nRefused if afterwards no active user would hold `system:admin`.",
    params(
        ("role" = String, Path, description = "Role name"),
        ("permission" = String, Path, description = "Permission name")
    ),
    responses(
        (status = 204, description = "Permission revoked successfully"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED): missing `role:manage`, or the permission or role is above the caller's ceiling", body = ErrorBody),
        (status = 404, description = "Role permission not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Would leave no active `system:admin` holder (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
pub async fn revoke_permission_from_role(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((role_name, permission_name)): AppPath<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::ROLE_MANAGE)?;
    // Before the 404 lookup, so a caller cannot probe which grants exist above
    // its ceiling.
    require_within_ceiling(
        &state.db,
        &auth_user,
        CeilingTarget::RolePermission {
            role: &role_name,
            permission: &permission_name,
        },
    )
    .await?;

    // Revoke + invalidate the in-flight access tokens of everyone holding this
    // role in one transaction, so a stale token cannot keep exercising the
    // permission that was just removed.
    let txn = state.db.begin().await?;
    let admins = SystemAdminLock::acquire(&txn).await?;

    let existing =
        role_permission::Entity::find_by_id((role_name.clone(), permission_name.clone()))
            .one(&txn)
            .await?;
    if existing.is_none() {
        return Err(AppError::NotFound("Role permission not found".into()));
    }

    admins
        .require_survivor(AdminSetChange::RevokeRolePermission {
            role: &role_name,
            permission: &permission_name,
        })
        .await?;

    role_permission::Entity::delete_by_id((role_name.clone(), permission_name))
        .exec(&txn)
        .await?;
    user::Entity::touch_credentials_changed_for_role(&txn, &role_name).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
