//! Transaction-scoped Postgres advisory locks keyed by a string.

use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, Statement};

/// Blocks until this transaction holds the advisory lock for `key`. The key
/// is hashed with `hashtextextended`, so any string names a lock; prefix it
/// with its owner to avoid collisions. Released on commit or rollback.
pub async fn lock_xact(txn: &DatabaseTransaction, key: &str) -> Result<(), DbErr> {
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        [key.into()],
    ))
    .await?;
    Ok(())
}
