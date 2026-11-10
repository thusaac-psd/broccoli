//! Authorization invariants shared by every handler that changes who holds
//! which permission.
//!
//! - [`require_within_ceiling`]: "you cannot grant, revoke, or act as what you
//!   do not hold". Used by user-role assign/revoke, user update/delete and
//!   role-permission grant/revoke.
//! - [`SystemAdminLock`]: keeps at least one active user holding
//!   `system:admin`. Used by every write that can shrink the holder set.
//!
//! # Lock order
//!
//! Every handler that takes locks takes them in this order, inside one
//! transaction:
//!
//! 1. [`SystemAdminLock::acquire`] (only paths that can shrink the holder set);
//! 2. the target user's row lock (`SELECT ... FOR UPDATE`), when the target is
//!    a user;
//! 3. then reads only: [`require_within_ceiling`], existence checks,
//!    [`SystemAdminLock::require_survivor`];
//! 4. the write.
//!
//! A ceiling check measured against a user runs after that user's row lock,
//! so it sees the target as it will be when the write commits: a concurrent
//! role assignment to the same user takes a `FOR KEY SHARE` lock on the user
//! row through the `user_role` foreign key, so it either commits before the
//! check reads or waits for this transaction.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement, Value};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::utils::advisory_lock;

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

/// Advisory-lock key serializing writes that can shrink the set of active
/// `system:admin` holders.
const SYSTEM_ADMIN_LOCK_KEY: &str = "system-admin-holders";

/// Whether any active user holds `$1` now, and whether one still would once
/// the grant paths matching `($2, $3, $4)` are gone. A grant path is one
/// `(user, role, permission)` row of the user -> role -> permission join. A
/// NULL pattern field matches anything. [`AdminSetChange::removed`] never
/// leaves all three NULL; [`SystemAdminLock::any_holder`] does and reads only
/// `held_now`.
const HOLDERS_SQL: &str = r#"
WITH holder AS (
    SELECT ur.user_id, ur.role, rp.permission
    FROM user_role ur
    JOIN "user" u ON u.id = ur.user_id AND u.deleted_at IS NULL
    JOIN role_permission rp ON rp.role = ur.role
    WHERE rp.permission = $1
)
SELECT
    EXISTS (SELECT 1 FROM holder) AS held_now,
    EXISTS (
        SELECT 1 FROM holder
        WHERE NOT (
            ($2::int4 IS NULL OR user_id = $2::int4)
            AND ($3::text IS NULL OR role = $3::text)
            AND ($4::text IS NULL OR permission = $4::text)
        )
    ) AS held_after
"#;

/// A pending write that may remove `system:admin` grant paths.
#[derive(Debug, Clone, Copy)]
pub enum AdminSetChange<'a> {
    /// Soft-deleting a user removes every path through that user.
    DeleteUser { user_id: i32 },
    /// Revoking one role from one user.
    RevokeUserRole { user_id: i32, role: &'a str },
    /// Revoking a permission from a role removes it from every holder of the
    /// role at once. Removes nothing unless `permission` is `system:admin`.
    RevokeRolePermission { role: &'a str, permission: &'a str },
}

impl AdminSetChange<'_> {
    /// `(user_id, role, permission)` pattern of the grant paths this change
    /// removes. `None` matches any value.
    fn removed(&self) -> (Option<i32>, Option<&str>, Option<&str>) {
        match *self {
            Self::DeleteUser { user_id } => (Some(user_id), None, None),
            Self::RevokeUserRole { user_id, role } => (Some(user_id), Some(role), None),
            Self::RevokeRolePermission { role, permission } => (None, Some(role), Some(permission)),
        }
    }

    fn refusal(&self) -> String {
        let system_admin = perm::SYSTEM_ADMIN;
        match self {
            Self::DeleteUser { .. } => {
                format!("Cannot delete user: they are the last active user holding {system_admin}")
            }
            Self::RevokeUserRole { role, .. } => format!(
                "Cannot revoke role \"{role}\": no active user would still hold {system_admin}"
            ),
            Self::RevokeRolePermission { role, .. } => format!(
                "Cannot revoke {system_admin} from role \"{role}\": no active user would still hold it"
            ),
        }
    }
}

/// Proof that this transaction holds the `system:admin` holder-set lock. The
/// survivor check is only reachable through it, so it cannot run unlocked.
///
/// Without the lock, two admins revoking each other at the same time would
/// each see the other as the surviving holder and both writes would commit.
/// Postgres runs these transactions at READ COMMITTED, so the check statement,
/// which starts after the lock is granted, sees whatever the previous holder
/// of the lock committed.
///
/// A new write path that can remove a holder (for example, deleting a role)
/// adds an [`AdminSetChange`] variant and goes through the same two calls.
pub struct SystemAdminLock<'t> {
    txn: &'t DatabaseTransaction,
}

impl<'t> SystemAdminLock<'t> {
    /// Takes the transaction-scoped lock. Call it before any row lock in the
    /// transaction (see the module docs for the lock order). Released on
    /// commit or rollback.
    pub async fn acquire(txn: &'t DatabaseTransaction) -> Result<Self, AppError> {
        advisory_lock::lock_xact(txn, SYSTEM_ADMIN_LOCK_KEY).await?;
        Ok(Self { txn })
    }

    /// Refuses with 409 `CONFLICT` if `change` would leave no active user
    /// holding `system:admin` when at least one holds it now. A system that
    /// already has no holder is not blocked from unrelated changes.
    pub async fn require_survivor(&self, change: AdminSetChange<'_>) -> Result<(), AppError> {
        let (held_now, held_after) = holders(self.txn, change.removed()).await?;
        if held_now && !held_after {
            tracing::warn!(
                ?change,
                "Refused a change that would leave no active system:admin holder"
            );
            return Err(AppError::Conflict(change.refusal()));
        }
        Ok(())
    }

    /// Whether any active user holds `system:admin`.
    pub async fn any_holder(&self) -> Result<bool, AppError> {
        Ok(holders(self.txn, (None, None, None)).await?.0)
    }
}

async fn holders(
    txn: &DatabaseTransaction,
    (user_id, role, permission): (Option<i32>, Option<&str>, Option<&str>),
) -> Result<(bool, bool), AppError> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            HOLDERS_SQL,
            [
                perm::SYSTEM_ADMIN.into(),
                user_id.into(),
                role.map(str::to_owned).into(),
                permission.map(str::to_owned).into(),
            ],
        ))
        .await?
        .ok_or_else(|| AppError::Internal("system:admin holder query returned no row".into()))?;
    Ok((row.try_get("", "held_now")?, row.try_get("", "held_after")?))
}
