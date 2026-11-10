//! The one place that mints credentials.
//!
//! Every endpoint that hands out an access token (`login`, `refresh`,
//! `cli-refresh`, `cli-token`, the device flow) goes through
//! [`issue_access_token`], which reads the user's CURRENT roles and
//! permissions from the database. Nothing copies grants out of a presented
//! JWT: re-signing old claims with a fresh `iat` would launder a token that
//! `FreshAuthUser` rejects (revoked role, reset password, deleted account)
//! into one it accepts.

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::error::AppError;
use crate::utils::jwt;

/// A freshly signed access token and the identity baked into it.
pub struct AccessGrant {
    pub token: String,
    pub user_id: i32,
    pub username: String,
    pub display_name: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}

/// The active user's profile plus their roles and the union of those roles'
/// permissions, in one round-trip. No row means the user does not exist or is
/// soft-deleted.
const GRANT_SQL: &str = r#"
SELECT
    u.username,
    u.display_name,
    to_jsonb(ARRAY(
        SELECT ur.role FROM user_role ur WHERE ur.user_id = u.id ORDER BY ur.role
    )) AS roles,
    to_jsonb(ARRAY(
        SELECT DISTINCT rp.permission
        FROM user_role ur JOIN role_permission rp ON rp.role = ur.role
        WHERE ur.user_id = u.id
        ORDER BY rp.permission
    )) AS permissions
FROM "user" u
WHERE u.id = $1 AND u.deleted_at IS NULL
"#;

/// Sign an access token for `user_id` from the user's current database state.
///
/// Returns `TokenInvalid` when the user is missing or soft-deleted, so a caller
/// that authenticated with an old credential cannot mint a token for an
/// account that no longer exists.
pub async fn issue_access_token<C: ConnectionTrait>(
    db: &C,
    jwt_secret: &str,
    user_id: i32,
) -> Result<AccessGrant, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            GRANT_SQL,
            [user_id.into()],
        ))
        .await?
        .ok_or(AppError::TokenInvalid)?;

    let username: String = row.try_get("", "username")?;
    let display_name: String = row.try_get("", "display_name")?;
    let roles: Vec<String> = json_strings(row.try_get("", "roles")?)?;
    let permissions: Vec<String> = json_strings(row.try_get("", "permissions")?)?;

    let token = jwt::sign_access_token(
        user_id,
        &username,
        roles.clone(),
        permissions.clone(),
        jwt_secret,
    )
    .map_err(|e| AppError::Internal(format!("JWT sign error: {e}")))?;

    Ok(AccessGrant {
        token,
        user_id,
        username,
        display_name,
        roles,
        permissions,
    })
}

fn json_strings(value: serde_json::Value) -> Result<Vec<String>, AppError> {
    serde_json::from_value(value)
        .map_err(|e| AppError::Internal(format!("malformed grant row: {e}")))
}
