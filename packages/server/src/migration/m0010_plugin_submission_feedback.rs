use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Upgrade the host's previous feedback protocol. Plugin data is copied
        // verbatim into metadata; its interpretation remains the plugin's job.
        manager.get_connection().execute_unprepared(r#"
            ALTER TABLE submission_feedback ADD COLUMN IF NOT EXISTS metadata JSONB;
            ALTER TABLE submission_feedback ADD COLUMN IF NOT EXISTS slot_key TEXT;
            ALTER TABLE submission_feedback ADD COLUMN IF NOT EXISTS public_result JSONB;
            UPDATE submission_feedback SET metadata = jsonb_build_object(
                'scope_key', scope_key, 'delay_seconds', delay_seconds,
                'effective_at_us', effective_at_us, 'previous_outcome', outcome)
                WHERE metadata IS NULL;
            UPDATE submission_feedback SET slot_key = contest_id || ':' || user_id
                WHERE slot_key IS NULL;
            ALTER TABLE submission_feedback DROP CONSTRAINT submission_feedback_outcome_check;
            UPDATE submission_feedback SET outcome = 'retired' WHERE outcome NOT IN ('pending', 'retired', 'released');
            ALTER TABLE submission_feedback ADD CONSTRAINT submission_feedback_outcome_check CHECK (outcome IN ('pending', 'retired', 'released'));
            ALTER TABLE submission_feedback ALTER COLUMN metadata SET NOT NULL;
            ALTER TABLE submission_feedback DROP COLUMN scope_key;
            ALTER TABLE submission_feedback DROP COLUMN delay_seconds;
            ALTER TABLE submission_feedback DROP COLUMN effective_at_us;
            DROP INDEX IF EXISTS submission_feedback_one_pending;
            CREATE UNIQUE INDEX submission_feedback_one_pending
                ON submission_feedback (plugin_id, slot_key)
                WHERE outcome = 'pending' AND slot_key IS NOT NULL;
            CREATE INDEX submission_feedback_slot ON submission_feedback (plugin_id, slot_key, submission_id);
            ALTER TABLE plugin_storage ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;
        "#).await?;
        Ok(())
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "opaque feedback metadata cannot be downgraded".into(),
        ))
    }
}
