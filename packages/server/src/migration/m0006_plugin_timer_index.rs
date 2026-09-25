use sea_orm_migration::prelude::*;

/// The partial index over unclaimed `plugin_timer` rows that keeps the
/// dispatcher's due-scan cheap - which the entity `sync()` cannot express.
///
/// The unique `(plugin_id, key)` constraint `timer_schedule` relies on is NOT
/// here: it is declared on the entity (`unique_key = "plugin_key"`). An earlier
/// version created it in this migration, and SeaORM's per-boot `sync()` dropped
/// it on the next restart because the entity did not declare it - silently
/// breaking every `timer_schedule` from then on. A partial index is not a
/// unique key, so `sync()` leaves the one below alone.
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
    // Partial: the table is empty most of the time, and only unclaimed rows
    // are ever scanned by fire_at (see dispatcher::plugin_timer::claim_due).
    r#"CREATE INDEX IF NOT EXISTS "plugin_timer_due_idx" ON "plugin_timer" ("fire_at") WHERE "claimed_at" IS NULL"#,
];

const REVERT_DDL: &[&str] = &[r#"DROP INDEX IF EXISTS "plugin_timer_due_idx""#];

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
