use sea_orm_migration::prelude::*;

/// Partial indexes over the `Queued` rows of the three durable-accept tables.
///
/// Every submit and code-run POST counts these rows for queue-depth admission
/// (`dispatcher::queue_depth::count_queued_rows`), and every claim tick picks
/// the oldest of them (`dispatcher::claim`, `WHERE status = 'Queued' ORDER BY
/// created_at`). Without an index both are sequential scans over tables that
/// only grow. Only `Queued` rows are indexed, so each index stays as small as
/// the queue-depth cap; `created_at` serves the claim order and the count
/// alike. The entity `sync()` cannot express a partial index, hence the
/// migration. `IF NOT EXISTS` keeps it idempotent.
#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLES: [&str; 3] = ["submission", "code_run", "submission_judgement"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in TABLES {
            db.execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS idx_{table}_queued ON {table} (created_at) \
                 WHERE status = 'Queued'"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in TABLES {
            db.execute_unprepared(&format!("DROP INDEX IF EXISTS idx_{table}_queued"))
                .await?;
        }
        Ok(())
    }
}
