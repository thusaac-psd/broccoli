//! Keeps at least one active user holding `system:admin`.
//!
//! Three writes can shrink the set of active `system:admin` holders:
//! soft-deleting a user, revoking a role from a user, and revoking a
//! permission from a role. Each of them, inside its own transaction:
//!
//! 1. calls [`SystemAdminLock::acquire`] before reading or locking any row;
//! 2. calls [`SystemAdminLock::require_survivor`] with the change it is about
//!    to make, which refuses with 409 `CONFLICT` if the change would take the
//!    holder set from non-empty to empty;
//! 3. makes the change and commits, which releases the lock.
//!
//! The advisory lock serializes those transactions. Without it, two admins
//! revoking each other at the same time would each see the other as the
//! surviving holder, and both writes would commit. Postgres runs these
//! transactions at READ COMMITTED, so the check statement, which starts after
//! the lock is granted, sees whatever the previous lock holder committed.
//!
//! A new write path that can remove a holder (for example, deleting a role)
//! adds a [`AdminSetChange`] variant and goes through the same two calls.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};

use crate::error::AppError;

/// Advisory-lock key, hashed the same way as the submission-slot locks in
/// `services::submission_feedback`.
const LOCK_KEY: &str = "system-admin-holders";

/// Whether any active user holds `$1` now, and whether one still would once
/// the grant paths matching `($2, $3, $4)` are gone. A grant path is one
/// `(user, role, permission)` row of the user -> role -> permission join. A
/// NULL pattern field matches anything; [`AdminSetChange::removed`] never
/// leaves all three NULL.
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
    /// Revoking a permission from a role, which removes it from every holder
    /// of that role at once. Removes nothing unless `permission` is
    /// `system:admin`.
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

/// Proof that this transaction holds the admin-set advisory lock. The
/// survivor check is only reachable through it, so it cannot run unlocked.
pub struct SystemAdminLock<'t> {
    txn: &'t DatabaseTransaction,
}

impl<'t> SystemAdminLock<'t> {
    /// Takes the transaction-scoped advisory lock. Call it before any row
    /// lock in the transaction, so every admin-set writer acquires locks in
    /// the same order. The lock is released on commit or rollback.
    pub async fn acquire(txn: &'t DatabaseTransaction) -> Result<Self, AppError> {
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [LOCK_KEY.into()],
        ))
        .await?;
        Ok(Self { txn })
    }

    /// Refuses with 409 `CONFLICT` if `change` would leave no active user
    /// holding `system:admin` when at least one holds it now. A system that
    /// already has no holder is not blocked from unrelated changes.
    pub async fn require_survivor(&self, change: AdminSetChange<'_>) -> Result<(), AppError> {
        let (user_id, role, permission) = change.removed();
        let row = self
            .txn
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
            .ok_or_else(|| {
                AppError::Internal("system:admin holder query returned no row".into())
            })?;
        let held_now: bool = row.try_get("", "held_now")?;
        let held_after: bool = row.try_get("", "held_after")?;

        if held_now && !held_after {
            tracing::warn!(
                ?change,
                "Refused a change that would leave no active system:admin holder"
            );
            return Err(AppError::Conflict(change.refusal()));
        }
        Ok(())
    }
}
