use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS submission_feedback (
                submission_id INTEGER PRIMARY KEY REFERENCES submission(id) ON DELETE CASCADE,
                contest_id INTEGER NOT NULL REFERENCES contest(id) ON DELETE CASCADE,
                user_id INTEGER NOT NULL REFERENCES \"user\"(id) ON DELETE CASCADE,
                plugin_id TEXT NOT NULL,
                state_key TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                effective_at_us BIGINT NOT NULL,
                delay_seconds BIGINT NOT NULL CHECK (delay_seconds >= 0),
                outcome TEXT NOT NULL DEFAULT 'pending'
                    CHECK (outcome IN ('pending', 'released', 'too_late'))
            );
            CREATE UNIQUE INDEX IF NOT EXISTS submission_feedback_one_pending
                ON submission_feedback (contest_id, user_id) WHERE outcome = 'pending';
            CREATE INDEX IF NOT EXISTS submission_feedback_match
                ON submission_feedback (plugin_id, state_key);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS submission_feedback")
            .await?;
        Ok(())
    }
}
