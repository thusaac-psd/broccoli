//! Authorization invariants shared by every handler that changes who holds
//! which permission.
//!
//! - [`require_within_ceiling`]: "you cannot grant, revoke, or act as what you
//!   do not hold". Used by user-role assign/revoke, user update/delete and
//!   role-permission grant/revoke.
//!
//! # Lock order
//!
//! A ceiling check measured against a user runs after that user's row lock
//! (`SELECT ... FOR UPDATE`), so it sees the target as it will be when the
//! write commits: a concurrent role assignment to the same user takes a
//! `FOR KEY SHARE` lock on the user row through the `user_role` foreign key,
//! so it either commits before the check reads or waits for this transaction.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;

/// What a privilege change is measured against.
#[derive(Debug, Clone, Copy)]
pub enum CeilingTarget<'a> {
    /// One permission (granting it to, or revoking it from, a role).
    Permission(&'a str),
    /// A role's permission set (assigning it to, or revoking it from, a user).
    Role(&'a str),
    /// A user's effective permissions, the union over all their roles
    /// (editing or deleting that account).
    User(i32),
}

/// Refuses with 403 `PERMISSION_DENIED` unless the caller holds every
/// permission of `target`. `system:admin` is the only root-equivalent grant
/// and bypasses the check without a query.
///
/// The subset test runs in SQL and returns at most one row, the first
/// permission the caller lacks.
pub async fn require_within_ceiling<C: ConnectionTrait>(
    db: &C,
    caller: &AuthUser,
    target: CeilingTarget<'_>,
) -> Result<(), AppError> {
    if caller.has_permission(perm::SYSTEM_ADMIN) {
        return Ok(());
    }

    let (scope, key): (&str, Value) = match target {
        CeilingTarget::Permission(permission) => {
            return if caller.has_permission(permission) {
                Ok(())
            } else {
                Err(refuse(caller, target, permission))
            };
        }
        CeilingTarget::Role(role) => ("rp.role = $1", role.into()),
        CeilingTarget::User(user_id) => (
            "rp.role IN (SELECT ur.role FROM user_role ur WHERE ur.user_id = $1)",
            user_id.into(),
        ),
    };
    let held = serde_json::to_value(&caller.permissions)
        .map_err(|e| AppError::Internal(format!("permission set encoding: {e}")))?;
    let sql = format!(
        "SELECT rp.permission FROM role_permission rp \
         WHERE {scope} \
         AND rp.permission NOT IN (SELECT jsonb_array_elements_text($2::jsonb)) \
         LIMIT 1"
    );
    let missing = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [key, held.into()],
        ))
        .await?;
    match missing {
        Some(row) => Err(refuse(
            caller,
            target,
            &row.try_get::<String>("", "permission")?,
        )),
        None => Ok(()),
    }
}

fn refuse(caller: &AuthUser, target: CeilingTarget<'_>, missing: &str) -> AppError {
    tracing::warn!(
        actor = caller.user_id,
        ?target,
        missing_permission = %missing,
        "Refused a change crossing the caller's privilege ceiling"
    );
    AppError::PermissionDenied
}
