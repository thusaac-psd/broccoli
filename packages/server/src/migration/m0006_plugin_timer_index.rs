use sea_orm_migration::prelude::*;

/// Indexes for `plugin_timer` that the entity `sync()` (which creates the
/// table itself) does not express: the unique `(plugin_id, key)` constraint
/// `timer_schedule` relies on for its `INSERT ... ON CONFLICT` replace
/// semantics, and a partial index over unclaimed rows that keeps the
/// dispatcher's due-scan cheap.
///
/// Idempotent (`IF NOT EXISTS`), so an existing deployment that already has
/// these indexes (e.g. from a prior manual apply) records this as a no-op.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0006_plugin_timer_index"
    }
}

const REQUIRED_DDL: &[&str] = &[
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "plugin_timer_plugin_key_idx" ON "plugin_timer" ("plugin_id", "key")"#,
    // Partial: the table is empty most of the time, and only unclaimed rows
    // are ever scanned by fire_at (see dispatcher::plugin_timer::claim_due).
    r#"CREATE INDEX IF NOT EXISTS "plugin_timer_due_idx" ON "plugin_timer" ("fire_at") WHERE "claimed_at" IS NULL"#,
];

const REVERT_DDL: &[&str] = &[
    r#"DROP INDEX IF EXISTS "plugin_timer_due_idx""#,
    r#"DROP INDEX IF EXISTS "plugin_timer_plugin_key_idx""#,
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for stmt in REQUIRED_DDL {
            db.execute_unprepared(stmt).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for stmt in REVERT_DDL {
            db.execute_unprepared(stmt).await?;
        }
        Ok(())
    }
}
